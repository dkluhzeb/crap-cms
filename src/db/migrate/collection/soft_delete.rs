//! The `soft_delete` transition of an existing collection table: the inline
//! `UNIQUE` constraints give way to the partial unique indexes the schema sync
//! manages, so a trashed row stops blocking a new one with the same value.
//!
//! Postgres drops the constraints in place. `SQLite` cannot, so the table is
//! rebuilt (see `super::rebuild`).

use std::collections::HashSet;

use anyhow::{Context as _, Result, bail};

use crate::{
    config::LocaleConfig,
    core::CollectionDefinition,
    db::{
        DbConnection, DbValue, migrate::helpers::collect_column_specs, query::helpers::quote_ident,
    },
};

/// Whether the stored table still has to lose an inline UNIQUE before the
/// partial unique indexes can take over: `soft_delete` is on for a table that
/// predates it (no `_deleted_at` column) and that carries a unique field.
///
/// Walks the flattened column specs, like the create and partial-index paths,
/// so a unique field nested in a group/row/tabs wrapper counts — otherwise its
/// stale inline UNIQUE survives and blocks re-inserting a value after its row
/// is soft-deleted.
///
/// The one reading of that condition: the schema sync asks it up front too,
/// because on `SQLite` the transition can only run with foreign keys off, and
/// that pragma has to be set before the sync transaction opens.
pub(in crate::db::migrate) fn soft_delete_transition_pending(
    def: &CollectionDefinition,
    existing: &HashSet<String>,
    locale_config: &LocaleConfig,
) -> bool {
    if !def.soft_delete || existing.contains("_deleted_at") {
        return false;
    }

    collect_column_specs(&def.fields, locale_config)
        .iter()
        .any(|spec| spec.field.unique && !spec.companion_text)
}

/// Refuse to serve a collection whose `soft_delete` was turned off while its
/// table still holds trashed rows.
///
/// The `_deleted_at IS NULL` filter every read appends is conditional on the
/// flag, so turning it off un-deletes every trashed document at once — in
/// lists, counts, the API and the admin UI — without a single write.
///
/// # Errors
///
/// Returns an error naming the collection and the number of trashed rows, or a
/// backend error if the count query fails.
pub(super) fn check_no_trashed_rows(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    existing: &HashSet<String>,
) -> Result<()> {
    if def.soft_delete || !existing.contains("_deleted_at") {
        return Ok(());
    }

    let sql = format!(
        "SELECT COUNT(*) AS cnt FROM {} WHERE _deleted_at IS NOT NULL",
        quote_ident(slug)
    );

    let trashed = conn
        .query_one(&sql, &[])
        .with_context(|| format!("Failed to count trashed rows in '{slug}'"))?
        .and_then(|row| row.get_i64("cnt").ok())
        .unwrap_or(0);

    if trashed == 0 {
        return Ok(());
    }

    bail!(
        "Collection '{slug}' has soft_delete turned off, but its table still holds {trashed} \
         trashed document(s) — every read would show them again. Turn soft_delete back on and \
         empty the trash (`crap-cms trash purge --collection {slug} -y`), or delete those rows \
         by hand, then start again."
    )
}

/// Drop every table-level UNIQUE constraint on `slug`.
///
/// `contype = 'u'` selects exactly those: the primary key is `'p'` and stays.
/// Nothing else about the table changes, so the junction tables and
/// `_versions_{slug}` keep both their rows and their foreign keys — which a
/// rebuild on this backend could not promise.
pub(super) fn drop_unique_constraints(conn: &dyn DbConnection, slug: &str) -> Result<()> {
    for name in unique_constraint_names(conn, slug)? {
        let sql = format!(
            "ALTER TABLE {} DROP CONSTRAINT {}",
            quote_ident(slug),
            quote_ident(&name)
        );

        conn.execute_ddl(&sql, &[])
            .with_context(|| format!("Failed to drop constraint '{name}' on '{slug}'"))?;
    }

    Ok(())
}

/// The catalog query for a table's UNIQUE constraints. The name is bound as a
/// text parameter and resolved with `to_regclass`, never `{placeholder}::regclass`:
/// an untyped parameter under an explicit cast is inferred as the cast target,
/// and the driver cannot bind a string to a `regclass`.
fn unique_constraints_sql(placeholder: &str) -> String {
    format!(
        "SELECT conname FROM pg_constraint \
         WHERE contype = 'u' AND conrelid = to_regclass({placeholder})"
    )
}

/// The names of `slug`'s UNIQUE constraints, read from the Postgres catalog.
/// The table name is passed as a quoted literal so `to_regclass` resolves it
/// verbatim rather than folding a capital away.
fn unique_constraint_names(conn: &dyn DbConnection, slug: &str) -> Result<Vec<String>> {
    let rows = conn.query_all(
        &unique_constraints_sql(&conn.placeholder(1)),
        &[DbValue::Text(quote_ident(slug))],
    )?;

    Ok(rows
        .iter()
        .filter_map(|r| r.get_string("conname").ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;
    use crate::core::{FieldDefinition, FieldType, Registry, RelationshipConfig};
    use crate::db::migrate::{
        collection::{
            alter::alter_collection_table, create::create_collection_table, sync_collection_table,
            test_helpers::*,
        },
        helpers::{get_table_columns, table_exists},
        sync_all,
    };

    /// A `posts` collection with a unique field and a has-many relationship,
    /// versioned — so its table has both a junction table and a versions table
    /// pointing at it with `ON DELETE CASCADE`.
    fn posts_with_children(soft_delete: bool) -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.soft_delete = soft_delete;
        def.versions = Some(VersionsConfig::new(true, 10));
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .unique(true)
                .build(),
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ];

        def
    }

    fn registry_with_posts(soft_delete: bool) -> Registry {
        let mut registry = Registry::new();
        registry.register_collection(CollectionDefinition::new("tags"));
        registry.register_collection(posts_with_children(soft_delete));

        registry
    }

    fn row_count(conn: &dyn DbConnection, table: &str) -> i64 {
        conn.query_one(&format!("SELECT COUNT(*) AS c FROM \"{table}\""), &[])
            .unwrap()
            .unwrap()
            .get_i64("c")
            .unwrap()
    }

    /// The table each child's `parent_id` / `_parent` foreign key points at.
    fn referenced_tables(conn: &dyn DbConnection, child: &str) -> Vec<String> {
        conn.query_all(&format!("PRAGMA foreign_key_list(\"{child}\")"), &[])
            .unwrap()
            .iter()
            .filter_map(|r| r.get_string("table").ok())
            .collect()
    }

    /// Turning on `soft_delete` replaces the inline UNIQUE with a partial
    /// unique index, and on `SQLite` that means rebuilding the table. The
    /// rebuild must not take the children with it.
    ///
    /// Renaming the original out of the way first repoints every child's
    /// `REFERENCES` clause at the temporary name — `SQLite` rewrites those
    /// whenever foreign keys are enabled — and dropping that table then runs an
    /// implicit `DELETE FROM`, firing the children's `ON DELETE CASCADE`: every
    /// junction row and every version row of the collection was deleted, and
    /// the children were left naming a table that no longer existed.
    #[test]
    fn the_soft_delete_transition_keeps_child_rows_and_their_foreign_keys() {
        let (_dir, pool) = in_memory_pool();

        sync_all(&pool, &registry_with_posts(false), &no_locale()).expect("initial sync");

        {
            let conn = pool.get().unwrap();
            conn.execute_batch(
                "INSERT INTO tags (id) VALUES ('t1');
                 INSERT INTO posts (id, title) VALUES ('p1', 'hello');
                 INSERT INTO posts_tags (parent_id, related_id, _order) \
                     VALUES ('p1', 't1', 0);
                 INSERT INTO _versions_posts \
                     (id, _parent, _version, _status, snapshot) \
                     VALUES ('v1', 'p1', 1, 'published', '{}');",
            )
            .unwrap();
        }

        sync_all(&pool, &registry_with_posts(true), &no_locale()).expect("soft-delete transition");

        let conn = pool.get().unwrap();

        assert_eq!(
            row_count(&conn, "posts_tags"),
            1,
            "the junction row must survive the rebuild"
        );
        assert_eq!(
            row_count(&conn, "_versions_posts"),
            1,
            "the version row must survive the rebuild"
        );
        assert_eq!(row_count(&conn, "posts"), 1, "the document itself survives");

        for child in ["posts_tags", "_versions_posts"] {
            assert_eq!(
                referenced_tables(&conn, child),
                vec!["posts".to_string()],
                "{child} must still reference the collection table, not the rebuild temporary"
            );
        }

        assert!(
            conn.query_all("PRAGMA foreign_key_check", &[])
                .unwrap()
                .is_empty(),
            "the rebuild must leave no dangling reference"
        );
        assert!(
            !table_exists(&conn, "_rebuild_posts").unwrap(),
            "the rebuild temporary must not outlive the transition"
        );

        // The point of the transition: a soft-deleted row no longer blocks a
        // new one with the same unique value.
        conn.execute(
            "UPDATE posts SET _deleted_at = '2026-01-01T00:00:00.000Z' WHERE id = 'p1'",
            &[],
        )
        .unwrap();
        conn.execute("INSERT INTO posts (id, title) VALUES ('p2', 'hello')", &[])
            .expect("the inline UNIQUE must be gone after the transition");
    }

    /// A `$1::regclass` parameter is inferred as `regclass` and cannot be
    /// bound from a string; the name has to go through `to_regclass`.
    #[test]
    fn the_constraint_catalog_query_binds_the_table_name_as_text() {
        let sql = unique_constraints_sql("$1");

        assert!(sql.contains("to_regclass($1)"), "{sql}");
        assert!(!sql.contains("::regclass"), "{sql}");
    }

    #[test]
    fn alter_rebuilds_table_to_remove_inline_unique_on_soft_delete_transition() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        // The table an older release created without soft_delete: the unique
        // field carries an inline UNIQUE.
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, slug TEXT UNIQUE, title TEXT, \
             _ref_count INTEGER NOT NULL DEFAULT 0, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        // Insert a row to verify data survives rebuild
        conn.execute(
            "INSERT INTO posts (id, slug, title) VALUES ('a', 'hello', 'Hello World')",
            &[],
        )
        .unwrap();

        // Enable soft_delete — should rebuild the table to remove inline UNIQUE
        let mut def2 = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .unique(true)
                    .build(),
                text_field("title"),
            ],
        );
        def2.soft_delete = true;
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        // Verify data survived
        let row = conn
            .query_one(
                "SELECT title FROM posts WHERE id = ?1",
                &[DbValue::Text("a".into())],
            )
            .unwrap();
        assert!(row.is_some(), "Data should survive table rebuild");

        // Verify inline UNIQUE is gone: soft-delete a row, then insert duplicate slug
        conn.execute(
            "UPDATE posts SET _deleted_at = '2025-01-01' WHERE id = 'a'",
            &[],
        )
        .unwrap();

        let result = conn.execute(
            "INSERT INTO posts (id, slug, title) VALUES ('b', 'hello', 'Hello Again')",
            &[],
        );
        assert!(
            result.is_ok(),
            "Inline UNIQUE should be removed — duplicate slug allowed when one row is soft-deleted"
        );
    }

    /// Regression: a unique field NESTED in a group (column `seo__slug`) also
    /// got an inline UNIQUE at create time in older releases, but the soft-delete rebuild trigger
    /// only inspected top-level `def.fields` — the group wrapper is not itself
    /// `unique`, so the rebuild was skipped and the stale inline UNIQUE survived,
    /// blocking re-insert after soft-delete. The trigger now walks the flattened
    /// column specs.
    #[test]
    fn alter_rebuilds_for_unique_field_nested_in_group_on_soft_delete_transition() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let group = || {
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("slug", FieldType::Text)
                        .unique(true)
                        .build(),
                ])
                .build()
        };

        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, seo__slug TEXT UNIQUE, title TEXT, \
             _ref_count INTEGER NOT NULL DEFAULT 0, created_at TEXT, updated_at TEXT)",
        )
        .unwrap();

        conn.execute(
            "INSERT INTO posts (id, seo__slug, title) VALUES ('a', 'hello', 'Hello')",
            &[],
        )
        .unwrap();

        let mut def2 = simple_collection("posts", vec![group(), text_field("title")]);
        def2.soft_delete = true;
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        // Soft-delete the row, then re-insert the same nested-unique value.
        conn.execute(
            "UPDATE posts SET _deleted_at = '2025-01-01' WHERE id = 'a'",
            &[],
        )
        .unwrap();

        let result = conn.execute(
            "INSERT INTO posts (id, seo__slug, title) VALUES ('b', 'hello', 'Again')",
            &[],
        );
        assert!(
            result.is_ok(),
            "inline UNIQUE on the nested `seo__slug` must be removed on the soft-delete transition"
        );
    }

    /// Regression: the soft-delete rebuild copied only the intersection of old
    /// and new columns, silently dropping orphan-column data (from a
    /// previously-removed field) — while the normal alter path preserves orphan
    /// columns (warns, never drops). The rebuild now re-adds orphan columns
    /// before copying so their data survives.
    #[test]
    fn rebuild_preserves_orphan_column_data() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        // Table with a unique field (so the rebuild fires) plus a field that
        // will be removed from the definition (becoming an orphan column).
        let def1 = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .unique(true)
                    .build(),
                text_field("legacy_note"),
            ],
        );
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();
        conn.execute(
            "INSERT INTO posts (id, slug, legacy_note) VALUES ('a', 's', 'keep me')",
            &[],
        )
        .unwrap();

        // Remove `legacy_note` from the def AND enable soft_delete → rebuild.
        let mut def2 = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .unique(true)
                    .build(),
            ],
        );
        def2.soft_delete = true;
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        // The orphan column and its data must survive the rebuild.
        let row = conn
            .query_one(
                "SELECT legacy_note FROM posts WHERE id = ?1",
                &[DbValue::Text("a".into())],
            )
            .unwrap()
            .expect("row survives rebuild");
        assert_eq!(
            row.get_opt_string("legacy_note").unwrap().as_deref(),
            Some("keep me"),
            "orphan-column data must not be dropped by the soft-delete rebuild"
        );
    }

    #[test]
    fn alter_does_not_rebuild_without_unique_fields() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        // Create collection without unique fields
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        // Enable soft_delete — no rebuild needed (no unique fields)
        let mut def2 = simple_collection("posts", vec![text_field("title")]);
        def2.soft_delete = true;
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("_deleted_at"));
    }

    /// Regression: turning `soft_delete` off dropped the `_deleted_at IS NULL`
    /// filter from every read, so every trashed document came back — a silent
    /// un-delete of the whole trash. The sync refuses to run instead.
    #[test]
    fn turning_soft_delete_off_with_trashed_rows_fails_the_sync() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let mut on = simple_collection("posts", vec![text_field("title")]);
        on.soft_delete = true;
        sync_collection_table(&conn, "posts", &on, &no_locale()).unwrap();

        conn.execute_batch(
            "INSERT INTO posts (id, title) VALUES ('live', 'Live');
             INSERT INTO posts (id, title, _deleted_at) VALUES ('gone', 'Gone', '2026-01-01');",
        )
        .unwrap();

        let off = simple_collection("posts", vec![text_field("title")]);
        let err = sync_collection_table(&conn, "posts", &off, &no_locale())
            .unwrap_err()
            .to_string();

        assert!(err.contains("posts"), "must name the collection: {err}");
        assert!(err.contains('1'), "must name the trashed count: {err}");
    }

    /// With the trash empty the flag can be turned off — the column may stay
    /// behind, it just holds no trashed row any more.
    #[test]
    fn turning_soft_delete_off_with_an_empty_trash_passes() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let mut on = simple_collection("posts", vec![text_field("title")]);
        on.soft_delete = true;
        sync_collection_table(&conn, "posts", &on, &no_locale()).unwrap();

        conn.execute_batch("INSERT INTO posts (id, title) VALUES ('live', 'Live');")
            .unwrap();

        let off = simple_collection("posts", vec![text_field("title")]);
        sync_collection_table(&conn, "posts", &off, &no_locale())
            .expect("an empty trash must not block turning soft_delete off");
    }
}
