//! B-tree index sync for collection tables.

use anyhow::{Context as _, Result, bail};
use std::collections::HashSet;
use tracing::info;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, collection::Auth},
    db::{
        DbConnection,
        migrate::helpers::collect_column_specs,
        query::{helpers::locale_column, is_valid_identifier},
    },
};

/// The naming prefix for every index this module manages: `idx_{slug}_`.
///
/// Load-bearing — `sync_indexes` only ever drops indexes whose name starts with
/// this prefix, so an index built with a different prefix can never be
/// recognized as stale (permanent orphan). One source shared by [`index_name`]
/// and the stale-drop scan so the two can't disagree.
fn index_prefix(slug: &str) -> String {
    format!("idx_{slug}_")
}

/// Build a managed index name — `idx_{slug}_{parts joined by _}` — so every
/// generator (field, soft-delete-unique, compound, auth-token) shares the exact
/// prefix the stale-drop scan matches on.
fn index_name(slug: &str, parts: &[&str]) -> String {
    format!("{}{}", index_prefix(slug), parts.join("_"))
}

/// Add an index entry to the desired set and create statement list.
fn add_index(
    desired: &mut HashSet<String>,
    stmts: &mut Vec<String>,
    idx_name: String,
    sql: String,
) {
    stmts.push(sql);
    desired.insert(idx_name);
}

/// Collect field-level indexes (index=true, skip if unique=true — already indexed).
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
                let sql = format!("CREATE INDEX IF NOT EXISTS {idx_name} ON {slug} ({col})");

                add_index(desired, stmts, idx_name, sql);
            }
        } else {
            let idx_name = index_name(slug, &[&spec.col_name]);
            let sql = format!(
                "CREATE INDEX IF NOT EXISTS {} ON {} ({})",
                idx_name, slug, spec.col_name
            );

            add_index(desired, stmts, idx_name, sql);
        }
    }

    Ok(())
}

/// Collect partial unique indexes for soft-delete collections.
fn collect_soft_delete_unique_indexes(
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
    desired: &mut HashSet<String>,
    stmts: &mut Vec<String>,
) -> Result<()> {
    if !def.soft_delete {
        return Ok(());
    }

    for spec in &collect_column_specs(&def.fields, locale_config) {
        if !spec.field.unique || spec.companion_text {
            continue;
        }

        if spec.is_localized {
            for locale in &locale_config.locales {
                let col = locale_column(&spec.col_name, locale)?;
                let idx_name = index_name(slug, &[&col, "active_unique"]);
                let sql = format!(
                    "CREATE UNIQUE INDEX IF NOT EXISTS {idx_name} ON {slug} ({col}) WHERE _deleted_at IS NULL"
                );

                add_index(desired, stmts, idx_name, sql);
            }
        } else {
            let idx_name = index_name(slug, &[&spec.col_name, "active_unique"]);
            let sql = format!(
                "CREATE UNIQUE INDEX IF NOT EXISTS {} ON {} ({}) WHERE _deleted_at IS NULL",
                idx_name, slug, spec.col_name
            );

            add_index(desired, stmts, idx_name, sql);
        }
    }

    Ok(())
}

/// Collect collection-level compound indexes.
fn collect_compound_indexes(
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
    desired: &mut HashSet<String>,
    stmts: &mut Vec<String>,
) -> Result<()> {
    let specs = collect_column_specs(&def.fields, locale_config);

    for index_def in &def.indexes {
        for field_name in &index_def.fields {
            if !is_valid_identifier(field_name) {
                bail!(
                    "Invalid field name '{field_name}' in compound index for collection '{slug}'"
                );
            }
        }

        let expanded_cols: Vec<String> = index_def
            .fields
            .iter()
            .map(|field_name| {
                let spec = specs.iter().find(|s| s.col_name == *field_name);

                match spec {
                    Some(s) if s.is_localized => {
                        locale_column(field_name, &locale_config.default_locale)
                    }
                    _ => Ok(field_name.clone()),
                }
            })
            .collect::<Result<Vec<String>>>()?;

        let col_list = expanded_cols.join(", ");
        let field_parts: Vec<&str> = index_def.fields.iter().map(String::as_str).collect();
        let idx_name = index_name(slug, &field_parts);
        let unique = if index_def.unique { "UNIQUE " } else { "" };
        let sql = format!("CREATE {unique}INDEX IF NOT EXISTS {idx_name} ON {slug} ({col_list})");

        add_index(desired, stmts, idx_name, sql);
    }

    Ok(())
}

/// Sync B-tree indexes for a collection table: field-level `index: true` and
/// collection-level compound `indexes`. Idempotent — creates missing indexes,
/// drops stale ones. Only manages indexes with the `idx_{slug}_` naming prefix.
/// Case-insensitive unique backstop for the auth identity column. The
/// validation-layer unique check compares emails with `LOWER() = LOWER()`
/// (matching `find_by_email`'s login lookup), but validation can be raced —
/// two concurrent registrations differing only in case would both pass and
/// then collide as one account at login. The expression index makes the
/// database itself enforce one account per case-folded email. Partial
/// (`WHERE _deleted_at IS NULL`) on soft-delete collections so deleted rows
/// don't block re-registration.
fn collect_auth_email_ci_index(
    slug: &str,
    def: &CollectionDefinition,
    desired: &mut HashSet<String>,
    stmts: &mut Vec<String>,
) {
    if !def.is_auth_collection() {
        return;
    }

    // Injected for every auth collection when absent, so it's always present;
    // guard anyway for hand-built definitions in tests.
    if !def.fields.iter().any(|f| f.name == "email") {
        return;
    }

    let idx_name = index_name(slug, &["email", "ci_unique"]);
    let partial = if def.soft_delete {
        " WHERE _deleted_at IS NULL"
    } else {
        ""
    };
    let sql =
        format!("CREATE UNIQUE INDEX IF NOT EXISTS {idx_name} ON {slug} (LOWER(email)){partial}");

    add_index(desired, stmts, idx_name, sql);
}

/// Index the auth token columns. Reset / verification flows look a user up
/// by `WHERE _reset_token = ?` / `WHERE _verification_token = ?`; without an
/// index those are full table scans of the user table on every attempt.
fn collect_auth_token_indexes(
    slug: &str,
    def: &CollectionDefinition,
    desired: &mut HashSet<String>,
    stmts: &mut Vec<String>,
) {
    if !def.is_auth_collection() {
        return;
    }

    let mut add = |col: &str| {
        let idx_name = index_name(slug, &[col]);
        desired.insert(idx_name.clone());
        stmts.push(format!(
            "CREATE INDEX IF NOT EXISTS {idx_name} ON {slug} ({col})"
        ));
    };

    add("_reset_token");

    if def.auth.as_ref().is_some_and(Auth::requires_verify_email) {
        add("_verification_token");
    }
}

pub(super) fn sync_indexes(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut desired: HashSet<String> = HashSet::new();
    let mut stmts: Vec<String> = Vec::new();

    collect_field_indexes(slug, def, locale_config, &mut desired, &mut stmts)?;
    collect_soft_delete_unique_indexes(slug, def, locale_config, &mut desired, &mut stmts)?;
    collect_compound_indexes(slug, def, locale_config, &mut desired, &mut stmts)?;
    collect_auth_token_indexes(slug, def, &mut desired, &mut stmts);
    collect_auth_email_ci_index(slug, def, &mut desired, &mut stmts);

    // Drop stale indexes (in existing but not in desired)
    let prefix = index_prefix(slug);
    let existing: HashSet<String> = conn.index_names(slug, &prefix)?.into_iter().collect();

    for name in existing.difference(&desired) {
        info!("Dropping stale index: {}", name);

        conn.execute_ddl(&format!("DROP INDEX IF EXISTS {name}"), &[])
            .with_context(|| format!("Failed to drop index {name}"))?;
    }

    // Create missing indexes
    for sql in &stmts {
        conn.execute_ddl(sql, &[])
            .with_context(|| format!("Failed to create index: {sql}"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;
    use crate::core::{FieldDefinition, FieldType};
    use crate::db::migrate::collection::create::create_collection_table;
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

    /// Regression (backstop): the app-level unique check compares emails
    /// case-insensitively, but nothing at the DB level enforced it — two
    /// concurrent registrations differing only in case could both land. The
    /// `LOWER(email)` unique index makes the database reject the second.
    #[test]
    fn sync_indexes_auth_email_ci_unique_rejects_case_variant_duplicate() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

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
        create_collection_table(&conn, "users", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "users", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "users");
        assert!(
            indexes.contains("idx_users_email_ci_unique"),
            "auth collection should get the case-insensitive email backstop; got {indexes:?}"
        );

        conn.execute(
            "INSERT INTO users (id, email) VALUES ('u1', 'Victim@x.com')",
            &[],
        )
        .unwrap();
        let dup = conn.execute(
            "INSERT INTO users (id, email) VALUES ('u2', 'victim@x.com')",
            &[],
        );
        assert!(
            dup.is_err(),
            "case-variant duplicate email must be rejected by the DB backstop"
        );
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

    #[test]
    fn sync_indexes_no_partial_unique_without_soft_delete() {
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
