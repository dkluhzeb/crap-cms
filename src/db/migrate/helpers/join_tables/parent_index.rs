//! The `parent_id` index of every array / blocks row table.
//!
//! Every read of a document's rows (`WHERE parent_id IN (…)` hydration), every
//! row-scoped filter (`EXISTS` / `NOT EXISTS` over the row table) and every
//! `ON DELETE CASCADE` from the parent looks rows up by `parent_id`. Neither
//! backend indexes a foreign-key column on its own, so without this index each
//! of those scans the whole row table. A localized row table is read per locale,
//! so its index leads with `parent_id` and adds `_locale`.
//!
//! Has-many relationship junctions need no extra index: their primary key
//! already leads with `parent_id`.

use anyhow::{Context as _, Result};
use sha2::{Digest, Sha256};

use crate::{
    core::hex::hex_encode,
    db::{DbConnection, query::helpers::quote_ident},
};

/// The naming prefix of a row table's `(parent_id)` index. The double
/// underscore keeps it disjoint from the per-collection `idx_{slug}_…` indexes
/// (a slug never starts with `_`) and from the version-table `idx__ver_…`
/// indexes.
const PREFIX: &str = "idx__rows_";

/// The naming prefix of a localized row table's `(parent_id, _locale)` index.
/// It differs from [`PREFIX`] right after `idx__`, so no two tables' indexes —
/// localized or not — can ever share a name.
const LOCALIZED_PREFIX: &str = "idx__lrows_";

/// The longest identifier Postgres keeps (`NAMEDATALEN - 1`); it silently cuts
/// a longer one, so two long names could end up the same.
const MAX_NAME_BYTES: usize = 63;

/// Hex digits of the table-name digest that ends a shortened index name.
const DIGEST_HEX: usize = 16;

/// The name of a row table's parent index: `idx__rows_{table}`, or
/// `idx__lrows_{table}` for a localized table, when that fits
/// [`MAX_NAME_BYTES`]. A longer one keeps the prefix and as much of the table
/// name as fits before `_` and 16 hex digits of the table name's SHA-256, so
/// every row table — however long its name — gets a deterministic index name
/// that both backends store unchanged and that no other table's shares.
fn row_parent_index_name(table: &str, localized: bool) -> String {
    let prefix = if localized { LOCALIZED_PREFIX } else { PREFIX };
    let full = format!("{prefix}{table}");

    if full.len() <= MAX_NAME_BYTES {
        return full;
    }

    let digest = hex_encode(&Sha256::digest(table.as_bytes())[..DIGEST_HEX / 2]);
    let head = char_prefix(table, MAX_NAME_BYTES - prefix.len() - 1 - DIGEST_HEX);

    format!("{prefix}{head}_{digest}")
}

/// The longest prefix of `s` of at most `max` bytes that ends on a character
/// boundary.
fn char_prefix(s: &str, max: usize) -> &str {
    let mut end = max.min(s.len());

    while !s.is_char_boundary(end) {
        end -= 1;
    }

    &s[..end]
}

/// Create the parent index a row table should have — `(parent_id, _locale)`
/// when localized, `(parent_id)` otherwise — and drop every other parent index
/// of the table, so a table that gains or loses its `_locale` column switches
/// index and an index created under an earlier name form is replaced.
/// Idempotent: safe on a new table and on every later sync of an existing one.
///
/// # Errors
///
/// Returns an error if listing the table's indexes, or a `DROP INDEX` /
/// `CREATE INDEX` statement, fails.
pub(super) fn sync_row_parent_index(
    conn: &dyn DbConnection,
    table: &str,
    localized: bool,
) -> Result<()> {
    let name = row_parent_index_name(table, localized);

    drop_other_parent_indexes(conn, table, &name)?;

    let columns = if localized {
        "parent_id, _locale"
    } else {
        "parent_id"
    };
    let sql = format!(
        "CREATE INDEX IF NOT EXISTS {} ON {} ({columns})",
        quote_ident(&name),
        quote_ident(table)
    );

    conn.execute_ddl(&sql, &[])
        .with_context(|| format!("Failed to create index {name}"))?;

    Ok(())
}

/// Drop every parent index of `table` except `keep`: the other locale shape's,
/// and one created under an earlier name form. They are found on the table
/// itself, so an index of another table is never touched.
fn drop_other_parent_indexes(conn: &dyn DbConnection, table: &str, keep: &str) -> Result<()> {
    for prefix in [PREFIX, LOCALIZED_PREFIX] {
        let names = conn
            .index_names(table, prefix)
            .with_context(|| format!("Failed to list the indexes of {table}"))?;

        for stale in names.iter().filter(|name| name.as_str() != keep) {
            conn.execute_ddl(&format!("DROP INDEX IF EXISTS {}", quote_ident(stale)), &[])
                .with_context(|| format!("Failed to drop index {stale}"))?;
        }
    }

    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::core::{BlockDefinition, CollectionDefinition, FieldDefinition, FieldType};
    use crate::db::{
        DbValue,
        migrate::{
            collection::{create_collection_table, test_helpers::*},
            helpers::join_tables::sync_join_tables,
        },
    };

    fn indexes(conn: &dyn DbConnection, table: &str) -> Vec<String> {
        let mut names = conn.index_names(table, "idx__").unwrap();
        names.sort();
        names
    }

    fn indexed_columns(conn: &dyn DbConnection, index: &str) -> Vec<String> {
        conn.query_all(
            "SELECT name FROM pragma_index_info(?1) ORDER BY seqno",
            &[DbValue::Text(index.to_string())],
        )
        .unwrap()
        .into_iter()
        .filter_map(|row| row.get_string("name").ok())
        .collect()
    }

    fn items(localized: bool) -> FieldDefinition {
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("label", FieldType::Text).build(),
            ])
            .localized(localized)
            .build()
    }

    fn content(localized: bool) -> FieldDefinition {
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new("text", vec![])])
            .localized(localized)
            .build()
    }

    #[test]
    fn names_are_disjoint_from_every_other_managed_index() {
        assert_eq!(
            row_parent_index_name("posts_items", false),
            "idx__rows_posts_items"
        );
        assert_eq!(
            row_parent_index_name("posts_items", true),
            "idx__lrows_posts_items"
        );

        // A localized table's name is never a plain table's name, whatever
        // the two tables are called.
        assert_ne!(
            row_parent_index_name("lposts_items", false),
            row_parent_index_name("posts_items", true)
        );

        for localized in [false, true] {
            let name = row_parent_index_name("posts_items", localized);
            assert!(!name.starts_with("idx__ver_"), "{name}");
            assert!(
                name.starts_with("idx__"),
                "{name}: a slug never starts with `_`"
            );
        }
    }

    /// Regression: array and blocks row tables had no index on `parent_id`, so
    /// every hydration, row filter and cascade delete scanned the whole table.
    #[test]
    fn new_array_and_blocks_tables_get_a_parent_index() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![items(false), content(false)]);

        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        for table in ["posts_items", "posts_content"] {
            let name = row_parent_index_name(table, false);
            assert_eq!(indexes(&conn, table), vec![name.clone()], "{table}");
            assert_eq!(indexed_columns(&conn, &name), vec!["parent_id"], "{table}");
        }
    }

    #[test]
    fn an_existing_table_gains_the_index_on_the_next_sync() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![items(false)]);

        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        conn.execute_ddl(
            "CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, \
             label TEXT)",
            &[],
        )
        .unwrap();
        assert!(indexes(&conn, "posts_items").is_empty());

        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert_eq!(
            indexes(&conn, "posts_items"),
            vec![row_parent_index_name("posts_items", false)]
        );
    }

    #[test]
    fn a_localized_table_indexes_parent_and_locale() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![items(true), content(true)]);

        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        for table in ["posts_items", "posts_content"] {
            let name = row_parent_index_name(table, true);
            assert_eq!(indexes(&conn, table), vec![name.clone()], "{table}");
            assert_eq!(
                indexed_columns(&conn, &name),
                vec!["parent_id", "_locale"],
                "{table}"
            );
        }
    }

    /// Enabling localization on a field adds `_locale` to its row table; the
    /// parent index follows, and the plain one is dropped.
    #[test]
    fn becoming_localized_swaps_the_index() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let plain = simple_collection("posts", vec![items(false)]);

        create_collection_table(&conn, "posts", &plain, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &plain.fields, &locale_en_de()).unwrap();

        let localized = simple_collection("posts", vec![items(true)]);
        sync_join_tables(&conn, "posts", &localized.fields, &locale_en_de()).unwrap();

        assert_eq!(
            indexes(&conn, "posts_items"),
            vec![row_parent_index_name("posts_items", true)]
        );
    }

    /// A name that would pass 63 bytes is shortened deterministically, stays
    /// distinct per table and per locale shape, and keeps its prefix.
    #[test]
    fn a_long_table_name_is_shortened_within_the_limit() {
        let long = format!("posts_{}", "f".repeat(50));
        let sibling = format!("posts_{}g", "f".repeat(49));

        for localized in [false, true] {
            let name = row_parent_index_name(&long, localized);

            assert!(name.len() <= MAX_NAME_BYTES, "{name}");
            assert_eq!(
                name,
                row_parent_index_name(&long, localized),
                "deterministic"
            );
            assert_ne!(name, row_parent_index_name(&sibling, localized), "{name}");
        }

        assert!(row_parent_index_name(&long, false).starts_with(PREFIX));
        assert!(row_parent_index_name(&long, true).starts_with(LOCALIZED_PREFIX));
        assert_ne!(
            row_parent_index_name(&long, false),
            row_parent_index_name(&long, true)
        );

        // A name that fits keeps the plain form.
        assert_eq!(
            row_parent_index_name("posts_items", false),
            "idx__rows_posts_items"
        );
    }

    /// `posts` with an array whose row table's index name would pass 63 bytes,
    /// and that row table's name.
    fn long_items() -> (CollectionDefinition, String) {
        let name = "f".repeat(50);
        let field = FieldDefinition::builder(&name, FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("label", FieldType::Text).build(),
            ])
            .build();

        (
            simple_collection("posts", vec![field]),
            format!("posts_{name}"),
        )
    }

    /// Regression: a row table whose index name passed 63 bytes refused to
    /// boot. It now gets its index under the shortened name.
    #[test]
    fn a_long_row_table_gets_its_parent_index() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let (def, table) = long_items();

        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let name = row_parent_index_name(&table, false);
        assert_eq!(indexes(&conn, &table), vec![name.clone()]);
        assert_eq!(indexed_columns(&conn, &name), vec!["parent_id"]);
    }

    /// An index created under an earlier name form of the same table — the
    /// full, unshortened name — is replaced by the current one.
    #[test]
    fn an_index_under_an_earlier_name_is_replaced() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let (def, table) = long_items();

        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let earlier = format!("{PREFIX}{table}");
        conn.execute_ddl(
            &format!(
                "CREATE INDEX {} ON {} (parent_id)",
                quote_ident(&earlier),
                quote_ident(&table)
            ),
            &[],
        )
        .unwrap();

        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert_eq!(
            indexes(&conn, &table),
            vec![row_parent_index_name(&table, false)]
        );
    }

    #[test]
    fn a_parent_lookup_uses_the_index() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![items(false)]);

        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let plan: Vec<String> = conn
            .query_all(
                "EXPLAIN QUERY PLAN SELECT id FROM posts_items WHERE parent_id = 'p'",
                &[],
            )
            .unwrap()
            .into_iter()
            .filter_map(|row| row.get_string("detail").ok())
            .collect();

        let name = row_parent_index_name("posts_items", false);
        assert!(plan.iter().any(|step| step.contains(&name)), "{plan:?}");
    }
}
