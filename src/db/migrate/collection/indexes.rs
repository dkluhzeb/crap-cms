//! B-tree index sync for collection tables.

use anyhow::{Context as _, Result, bail};
use std::collections::HashSet;
use tracing::info;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, FieldDefinition, IndexDefinition, collection::Auth},
    db::{
        DbConnection,
        migrate::helpers::collect_column_specs,
        query::{
            helpers::{locale_column, quote_ident},
            is_valid_identifier,
        },
    },
};

/// The naming prefix for every index this module manages: `idx_{slug}_`.
///
/// Load-bearing — `sync_indexes` only ever drops indexes whose name starts with
/// this prefix, so an index built with a different prefix can never be
/// recognized as stale (permanent orphan). One source shared by [`index_name`]
/// and the stale-drop scan so the two can't disagree.
pub(super) fn index_prefix(slug: &str) -> String {
    format!("idx_{slug}_")
}

/// Build a managed index name — `idx_{slug}_{parts joined by _}` — so every
/// generator (field, soft-delete-unique, compound, auth-token) shares the exact
/// prefix the stale-drop scan matches on.
fn index_name(slug: &str, parts: &[&str]) -> String {
    format!("{}{}", index_prefix(slug), parts.join("_"))
}

/// Add an index entry to the desired set and create statement list. Two indexes
/// of one table can't share a name: `CREATE … IF NOT EXISTS` would silently skip
/// the second.
///
/// # Errors
///
/// Returns an error naming the index when its name is already taken.
fn add_index(
    desired: &mut HashSet<String>,
    stmts: &mut Vec<String>,
    idx_name: String,
    sql: String,
) -> Result<()> {
    if desired.contains(&idx_name) {
        bail!(
            "Two indexes would both be named '{idx_name}' — rename a field or change a compound \
             index so their names differ"
        );
    }

    desired.insert(idx_name);
    stmts.push(sql);

    Ok(())
}

/// Collect field-level indexes (index=true, skip if unique=true — the managed
/// unique index below already indexes the column).
fn collect_field_indexes(
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
    desired: &mut HashSet<String>,
    stmts: &mut Vec<String>,
) -> Result<()> {
    for spec in &collect_column_specs(&def.fields, locale_config) {
        if !spec.field.index || spec.field.unique {
            continue;
        }

        if spec.is_localized {
            for locale in &locale_config.locales {
                let col = locale_column(&spec.col_name, locale)?;
                let idx_name = index_name(slug, &[&col]);
                let sql = format!(
                    "CREATE INDEX IF NOT EXISTS {} ON {slug} ({})",
                    quote_ident(&idx_name),
                    quote_ident(&col)
                );

                add_index(desired, stmts, idx_name, sql)?;
            }
        } else {
            let idx_name = index_name(slug, &[&spec.col_name]);
            let sql = format!(
                "CREATE INDEX IF NOT EXISTS {} ON {} ({})",
                quote_ident(&idx_name),
                slug,
                quote_ident(&spec.col_name)
            );

            add_index(desired, stmts, idx_name, sql)?;
        }
    }

    Ok(())
}

/// The name and `CREATE` statement of one column's managed unique index.
///
/// Partial (`WHERE _deleted_at IS NULL`) on a soft-delete collection so
/// trashed rows don't keep their values reserved; a plain unique index
/// otherwise.
fn unique_index(slug: &str, col: &str, soft_delete: bool) -> (String, String) {
    let (suffix, filter) = if soft_delete {
        ("active_unique", " WHERE _deleted_at IS NULL")
    } else {
        ("unique", "")
    };

    let idx_name = index_name(slug, &[col, suffix]);
    let sql = format!(
        "CREATE UNIQUE INDEX IF NOT EXISTS {} ON {slug} ({}){filter}",
        quote_ident(&idx_name),
        quote_ident(col)
    );

    (idx_name, sql)
}

/// Collect the unique index of every `unique` field.
///
/// Uniqueness lives here for ALL collections rather than as an inline `UNIQUE`
/// at CREATE: an inline constraint can only be written once, so a field that
/// gains `unique` after its column exists would never get one and the database
/// would leave the rule to the validation layer alone.
fn collect_unique_indexes(
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
    desired: &mut HashSet<String>,
    stmts: &mut Vec<String>,
) -> Result<()> {
    for spec in &collect_column_specs(&def.fields, locale_config) {
        if !spec.field.unique || spec.companion_text {
            continue;
        }

        if spec.is_localized {
            for locale in &locale_config.locales {
                let col = locale_column(&spec.col_name, locale)?;
                let (idx_name, sql) = unique_index(slug, &col, def.soft_delete);

                add_index(desired, stmts, idx_name, sql)?;
            }
        } else {
            let (idx_name, sql) = unique_index(slug, &spec.col_name, def.soft_delete);

            add_index(desired, stmts, idx_name, sql)?;
        }
    }

    Ok(())
}

/// The columns a compound index spans: a localized field is indexed by its
/// default locale's column.
///
/// # Errors
///
/// Returns an error if the default locale code has no column form.
pub(in crate::db::migrate) fn compound_index_columns(
    fields: &[FieldDefinition],
    index_def: &IndexDefinition,
    locale_config: &LocaleConfig,
) -> Result<Vec<String>> {
    let specs = collect_column_specs(fields, locale_config);

    index_def
        .fields
        .iter()
        .map(
            |field_name| match specs.iter().find(|s| s.col_name == *field_name) {
                Some(s) if s.is_localized => {
                    locale_column(field_name, &locale_config.default_locale)
                }
                _ => Ok(field_name.clone()),
            },
        )
        .collect()
}

/// Collect collection-level compound indexes.
fn collect_compound_indexes(
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
    desired: &mut HashSet<String>,
    stmts: &mut Vec<String>,
) -> Result<()> {
    for index_def in &def.indexes {
        for field_name in &index_def.fields {
            if !is_valid_identifier(field_name) {
                bail!(
                    "Invalid field name '{field_name}' in compound index for collection '{slug}'"
                );
            }
        }

        let expanded_cols = compound_index_columns(&def.fields, index_def, locale_config)?;

        let col_list = expanded_cols
            .iter()
            .map(|col| quote_ident(col))
            .collect::<Vec<_>>()
            .join(", ");
        let field_parts: Vec<&str> = index_def.fields.iter().map(String::as_str).collect();
        let idx_name = index_name(slug, &field_parts);
        let unique = if index_def.unique { "UNIQUE " } else { "" };
        let sql = format!(
            "CREATE {unique}INDEX IF NOT EXISTS {} ON {slug} ({col_list})",
            quote_ident(&idx_name)
        );

        add_index(desired, stmts, idx_name, sql)?;
    }

    Ok(())
}

/// Index the auth token columns. Reset / verification flows look a user up
/// by `WHERE _reset_token = ?` / `WHERE _verification_token = ?`; without an
/// index those are full table scans of the user table on every attempt.
fn collect_auth_token_indexes(
    slug: &str,
    def: &CollectionDefinition,
    desired: &mut HashSet<String>,
    stmts: &mut Vec<String>,
) -> Result<()> {
    if !def.is_auth_collection() {
        return Ok(());
    }

    let mut columns = vec!["_reset_token"];
    if def.auth.as_ref().is_some_and(Auth::requires_verify_email) {
        columns.push("_verification_token");
    }

    for col in columns {
        let idx_name = index_name(slug, &[col]);
        let sql = format!(
            "CREATE INDEX IF NOT EXISTS {} ON {slug} ({})",
            quote_ident(&idx_name),
            quote_ident(col)
        );

        add_index(desired, stmts, idx_name, sql)?;
    }

    Ok(())
}

/// The indexes a collection's table should have: their names and their
/// `CREATE` statements.
fn desired_indexes(
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<(HashSet<String>, Vec<String>)> {
    let mut desired: HashSet<String> = HashSet::new();
    let mut stmts: Vec<String> = Vec::new();

    collect_field_indexes(slug, def, locale_config, &mut desired, &mut stmts)?;
    collect_unique_indexes(slug, def, locale_config, &mut desired, &mut stmts)?;
    collect_compound_indexes(slug, def, locale_config, &mut desired, &mut stmts)?;
    collect_auth_token_indexes(slug, def, &mut desired, &mut stmts)?;

    Ok((desired, stmts))
}

/// The names of every index the migration manages on a collection's table.
///
/// # Errors
///
/// Returns an error if a compound index names an invalid field or a locale
/// column name can't be built.
pub(in crate::db::migrate) fn managed_index_names(
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<HashSet<String>> {
    Ok(desired_indexes(slug, def, locale_config)?.0)
}

/// Drop the managed indexes of a collection's table its definition no longer
/// asks for. Only indexes with the `idx_{slug}_` naming prefix are managed.
///
/// Runs with the table sync, before the conversions rewrite stored values: an
/// index the definition dropped (a field that lost `unique`) must not reject a
/// rewrite the definition allows.
pub(super) fn drop_stale_indexes(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let desired = managed_index_names(slug, def, locale_config)?;
    let prefix = index_prefix(slug);
    let existing: HashSet<String> = conn.index_names(slug, &prefix)?.into_iter().collect();

    for name in existing.difference(&desired) {
        info!("Dropping stale index: {}", name);

        // The name comes from the database catalog, where it may hold any
        // character after the managed prefix — always quote it.
        conn.execute_ddl(&format!("DROP INDEX IF EXISTS {}", quote_ident(name)), &[])
            .with_context(|| format!("Failed to drop index {name}"))?;
    }

    Ok(())
}

/// Create the managed indexes a collection's table is missing: field-level
/// `index` / `unique`, collection-level compound `indexes` and the auth token
/// indexes. Idempotent.
///
/// A unique index depends on the stored values, so it is created only after
/// the conversions that rewrite them: the canonical-text pass reports values
/// that are the same once canonical with the documents holding them, which a
/// failing `CREATE UNIQUE INDEX` over the values as they were stored could
/// not.
pub(in crate::db::migrate) fn create_indexes(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let (_, stmts) = desired_indexes(slug, def, locale_config)?;

    for sql in &stmts {
        conn.execute_ddl(sql, &[])
            .with_context(|| format!("Failed to create index: {sql}"))?;
    }

    Ok(())
}

/// Both halves of the index sync, back to back — what a test of one table's
/// indexes needs.
#[cfg(test)]
fn sync_indexes(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    drop_stale_indexes(conn, slug, def, locale_config)?;
    create_indexes(conn, slug, def, locale_config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;
    use crate::core::normalize_email;
    use crate::core::{FieldDefinition, FieldType};
    use crate::db::migrate::collection::create::create_collection_table;
    use crate::db::migrate::collection::sync_collection_table;
    use crate::db::migrate::collection::test_helpers::*;
    use crate::db::{DbConnection, DbValue};

    fn get_indexes(conn: &dyn DbConnection, table: &str) -> HashSet<String> {
        conn.query_all(
            "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name=?1",
            &[DbValue::Text(table.to_string())],
        )
        .unwrap()
        .into_iter()
        .filter_map(|r| r.get_string("name").ok())
        .collect()
    }

    /// Regression: a field index and a compound index over the same column got
    /// one name, and `CREATE … IF NOT EXISTS` silently skipped the second —
    /// here the unique one, so uniqueness went unenforced.
    #[test]
    fn indexes_sharing_a_name_are_rejected() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .index(true)
                .build(),
        ];
        let mut compound = IndexDefinition::new(vec!["title".to_string()]);
        compound.unique = true;
        def.indexes = vec![compound];

        let err = desired_indexes("posts", &def, &LocaleConfig::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("idx_posts_title"), "{err}");
    }

    /// Regression: every managed index name must start with `index_prefix`.
    /// The stale-drop scan only drops names matching that prefix, so a builder
    /// that produced a differently-prefixed name would leave a permanent orphan.
    /// Pinning the invariant at the naming source keeps the four generators and
    /// the drop scan from drifting apart.
    #[test]
    fn index_name_always_carries_the_drop_prefix() {
        let prefix = index_prefix("posts");
        assert_eq!(prefix, "idx_posts_");

        for parts in [
            vec!["title"],
            vec!["slug", "active_unique"],
            vec!["a", "b", "c"],
            vec!["_reset_token"],
        ] {
            let name = index_name("posts", &parts);
            assert!(name.starts_with(&prefix), "{name} must start with {prefix}");
        }
    }

    /// Index DDL quotes names that carry a capital — a locale code such as
    /// `de-DE` — so Postgres creates and drops the index it was asked for.
    #[test]
    fn index_ddl_quotes_uppercase_locale_names() {
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("title", FieldType::Text)
                    .localized(true)
                    .index(true)
                    .build(),
            ],
        );
        let locales = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de-DE".to_string()],
            fallback: true,
        };

        let mut desired = HashSet::new();
        let mut stmts = Vec::new();
        collect_field_indexes("posts", &def, &locales, &mut desired, &mut stmts).unwrap();

        assert!(desired.contains("idx_posts_title__de_DE"), "{desired:?}");
        assert!(
            stmts.contains(
                &"CREATE INDEX IF NOT EXISTS \"idx_posts_title__de_DE\" ON posts (\"title__de_DE\")"
                    .to_string()
            ),
            "{stmts:?}"
        );
    }

    #[test]
    fn sync_indexes_creates_auth_token_indexes() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("users", vec![]);
        def.auth = Some(Auth::new(true));
        create_collection_table(&conn, "users", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "users", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "users");
        assert!(
            indexes.contains("idx_users__reset_token"),
            "auth collection should index _reset_token; got {indexes:?}"
        );
        // verify_email is off by default → no verification-token index.
        assert!(!indexes.contains("idx_users__verification_token"));
    }

    /// An auth collection whose `email` column holds canonical addresses.
    fn users_def() -> CollectionDefinition {
        let mut def = simple_collection(
            "users",
            vec![
                FieldDefinition::builder("email", FieldType::Email)
                    .required(true)
                    .unique(true)
                    .build(),
            ],
        );
        def.auth = Some(Auth::new(true));

        def
    }

    /// Every write stores an email in its canonical form, so the email field's
    /// own unique index is the database's backstop against two accounts for
    /// one address — compared exactly the way the application compares it.
    /// Regression: a separate `LOWER(email)` index folded case by the
    /// database's rules instead, which need not agree with the canonical form
    /// (Postgres folds by the database's locale, `SQLite` only ASCII).
    #[test]
    fn auth_email_uniqueness_is_the_canonical_form() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = users_def();
        create_collection_table(&conn, "users", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "users", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "users");
        assert!(indexes.contains("idx_users_email_unique"), "{indexes:?}");
        assert!(
            !indexes.contains("idx_users_email_ci_unique"),
            "{indexes:?}"
        );

        let insert = |id: &str, email: &str| {
            conn.execute(
                "INSERT INTO users (id, email) VALUES (?1, ?2)",
                &[
                    DbValue::Text(id.to_string()),
                    DbValue::Text(normalize_email(email)),
                ],
            )
        };

        insert("u1", "Victim@x.com").unwrap();
        insert("u2", "\u{c4}RGER@x.com").unwrap();
        insert("u3", "stra\u{df}e@x.com").unwrap();

        for (id, dup) in [("u4", "victim@X.COM"), ("u5", "a\u{308}rger@x.com")] {
            assert!(insert(id, dup).is_err(), "{dup:?} is the same account");
        }

        insert("u6", "strasse@x.com").expect("a different canonical address is a new account");
    }

    /// The case-folding email index an earlier build created is dropped as
    /// stale.
    #[test]
    fn the_case_folding_email_index_is_dropped() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = users_def();
        create_collection_table(&conn, "users", &def, &no_locale()).unwrap();
        conn.execute(
            "CREATE UNIQUE INDEX idx_users_email_ci_unique ON users (LOWER(email))",
            &[],
        )
        .unwrap();

        sync_indexes(&conn, "users", &def, &no_locale()).unwrap();

        assert!(!get_indexes(&conn, "users").contains("idx_users_email_ci_unique"));
    }

    #[test]
    fn sync_indexes_skips_auth_tokens_for_non_auth_collection() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(!indexes.contains("idx_posts__reset_token"));
    }

    #[test]
    fn sync_indexes_creates_index_for_indexed_field() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("status", FieldType::Text)
                    .index(true)
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(
            indexes.contains("idx_posts_status"),
            "Should create index for index=true field"
        );
    }

    #[test]
    fn sync_indexes_skips_unique_field() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .unique(true)
                    .index(true) // should be skipped because unique=true
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(
            !indexes.contains("idx_posts_slug"),
            "Should skip index when unique=true"
        );
    }

    #[test]
    fn sync_indexes_creates_compound_index() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def =
            simple_collection("posts", vec![text_field("status"), text_field("category")]);
        def.indexes = vec![IndexDefinition {
            fields: vec!["status".to_string(), "category".to_string()],
            unique: false,
        }];
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(
            indexes.contains("idx_posts_status_category"),
            "Should create compound index"
        );
    }

    #[test]
    fn sync_indexes_creates_compound_unique_index() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![text_field("category"), text_field("slug")]);
        def.indexes = vec![IndexDefinition {
            fields: vec!["category".to_string(), "slug".to_string()],
            unique: true,
        }];
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(
            indexes.contains("idx_posts_category_slug"),
            "Should create compound unique index"
        );
    }

    #[test]
    fn sync_indexes_drops_stale_indexes() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def =
            simple_collection("posts", vec![text_field("status"), text_field("category")]);
        def.indexes = vec![IndexDefinition {
            fields: vec!["status".to_string()],
            unique: false,
        }];
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();
        assert!(get_indexes(&conn, "posts").contains("idx_posts_status"));

        // Remove the compound index, add a different one
        def.indexes = vec![IndexDefinition {
            fields: vec!["category".to_string()],
            unique: false,
        }];
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(
            !indexes.contains("idx_posts_status"),
            "Old index should be dropped"
        );
        assert!(
            indexes.contains("idx_posts_category"),
            "New index should be created"
        );
    }

    #[test]
    fn sync_indexes_localized_field() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("title", FieldType::Text)
                    .localized(true)
                    .index(true)
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_indexes(&conn, "posts", &def, &locale_en_de()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(
            indexes.contains("idx_posts_title__en"),
            "Should create index per locale: en"
        );
        assert!(
            indexes.contains("idx_posts_title__de"),
            "Should create index per locale: de"
        );
    }

    #[test]
    fn sync_indexes_idempotent() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("status", FieldType::Text)
                    .index(true)
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        // Run twice — should not error
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(indexes.contains("idx_posts_status"));
    }

    #[test]
    fn sync_indexes_validates_compound_field_names() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.indexes = vec![IndexDefinition {
            fields: vec!["1=1; DROP TABLE posts; --".to_string()],
            unique: false,
        }];
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let result = sync_indexes(&conn, "posts", &def, &no_locale());
        assert!(
            result.is_err(),
            "Should reject invalid identifier in compound index"
        );
    }

    #[test]
    fn sync_indexes_creates_partial_unique_for_soft_delete() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .unique(true)
                    .build(),
            ],
        );
        def.soft_delete = true;
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(
            indexes.contains("idx_posts_slug_active_unique"),
            "Should create partial unique index for soft-delete collection: {indexes:?}"
        );
    }

    /// Without soft delete the unique index is the full one — no
    /// `_deleted_at` predicate, since the column doesn't exist.
    #[test]
    fn sync_indexes_creates_full_unique_without_soft_delete() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .unique(true)
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(
            !indexes.contains("idx_posts_slug_active_unique"),
            "Should NOT create partial unique index for non-soft-delete collection"
        );
        assert!(
            indexes.contains("idx_posts_slug_unique"),
            "Should create the managed unique index: {indexes:?}"
        );

        conn.execute("INSERT INTO posts (id, slug) VALUES ('a', 'hello')", &[])
            .unwrap();
        assert!(
            conn.execute("INSERT INTO posts (id, slug) VALUES ('b', 'hello')", &[])
                .is_err(),
            "the managed unique index must block a duplicate"
        );
    }

    /// Regression: `unique` added to a field whose column already exists was
    /// never enforced by the database — the inline `UNIQUE` could only be
    /// written at CREATE, and the index collector skipped `unique` fields on
    /// the assumption it had been. The next sync must create the index.
    #[test]
    fn unique_added_to_an_existing_column_is_enforced_on_the_next_sync() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let plain = simple_collection(
            "posts",
            vec![FieldDefinition::builder("slug", FieldType::Text).build()],
        );
        sync_collection_table(&conn, "posts", &plain, &no_locale()).unwrap();

        conn.execute("INSERT INTO posts (id, slug) VALUES ('a', 'hello')", &[])
            .unwrap();

        let unique = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .unique(true)
                    .build(),
            ],
        );
        sync_collection_table(&conn, "posts", &unique, &no_locale()).unwrap();
        create_indexes(&conn, "posts", &unique, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(
            indexes.contains("idx_posts_slug_unique"),
            "adding unique must create the managed index: {indexes:?}"
        );
        assert!(
            conn.execute("INSERT INTO posts (id, slug) VALUES ('b', 'hello')", &[])
                .is_err(),
            "a duplicate must fail at the DB level once unique was added"
        );
    }

    #[test]
    fn partial_unique_index_allows_duplicate_in_deleted_rows() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .unique(true)
                    .build(),
            ],
        );
        def.soft_delete = true;
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        // Insert a soft-deleted row
        conn.execute(
            "INSERT INTO posts (id, slug, _deleted_at) VALUES ('a', 'hello', '2025-01-01')",
            &[],
        )
        .unwrap();

        // Insert an active row with the same slug — should succeed
        let result = conn.execute("INSERT INTO posts (id, slug) VALUES ('b', 'hello')", &[]);
        assert!(
            result.is_ok(),
            "Partial unique index should allow same value in deleted + active rows"
        );
    }

    #[test]
    fn partial_unique_index_blocks_duplicate_active_rows() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .unique(true)
                    .build(),
            ],
        );
        def.soft_delete = true;
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        conn.execute("INSERT INTO posts (id, slug) VALUES ('a', 'hello')", &[])
            .unwrap();

        let result = conn.execute("INSERT INTO posts (id, slug) VALUES ('b', 'hello')", &[]);
        assert!(
            result.is_err(),
            "Partial unique index should still block duplicate active rows"
        );
    }

    #[test]
    fn sync_indexes_creates_partial_unique_for_localized_field() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .unique(true)
                    .localized(true)
                    .build(),
            ],
        );
        def.soft_delete = true;
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_indexes(&conn, "posts", &def, &locale_en_de()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(
            indexes.contains("idx_posts_slug__en_active_unique"),
            "Should create partial unique index per locale: {indexes:?}"
        );
        assert!(
            indexes.contains("idx_posts_slug__de_active_unique"),
            "Should create partial unique index per locale: {indexes:?}"
        );
    }
}
