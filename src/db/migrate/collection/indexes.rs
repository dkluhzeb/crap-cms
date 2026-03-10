//! B-tree index sync for collection tables.

use anyhow::{Context as _, Result};
use std::collections::HashSet;

use crate::config::LocaleConfig;
use crate::db::migrate::helpers::{collect_column_specs, sanitize_locale};

/// Sync B-tree indexes for a collection table: field-level `index: true` and
/// collection-level compound `indexes`. Idempotent — creates missing indexes,
/// drops stale ones. Only manages indexes with the `idx_{slug}_` naming prefix.
pub(super) fn sync_indexes(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &crate::core::CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut desired: HashSet<String> = HashSet::new();
    let mut create_stmts: Vec<String> = Vec::new();

    // 1. Field-level indexes: index=true (skip if unique=true — SQLite already indexes those)
    for spec in &collect_column_specs(&def.fields, locale_config) {
        if !spec.field.index || spec.field.unique {
            continue;
        }
        if spec.is_localized {
            for locale in &locale_config.locales {
                let col = format!("{}__{}", spec.col_name, sanitize_locale(locale));
                let idx_name = format!("idx_{}_{}", slug, col);
                create_stmts.push(format!(
                    "CREATE INDEX IF NOT EXISTS {} ON {} ({})",
                    idx_name, slug, col
                ));
                desired.insert(idx_name);
            }
        } else {
            let idx_name = format!("idx_{}_{}", slug, spec.col_name);
            create_stmts.push(format!(
                "CREATE INDEX IF NOT EXISTS {} ON {} ({})",
                idx_name, slug, spec.col_name
            ));
            desired.insert(idx_name);
        }
    }

    // 2. Collection-level compound indexes
    for index_def in &def.indexes {
        // Validate all field names
        for field_name in &index_def.fields {
            if !crate::db::query::is_valid_identifier(field_name) {
                anyhow::bail!(
                    "Invalid field name '{}' in compound index for collection '{}'",
                    field_name, slug
                );
            }
        }

        // Expand localized fields to locale-specific columns
        let specs = collect_column_specs(&def.fields, locale_config);
        let mut expanded_cols: Vec<String> = Vec::new();
        for field_name in &index_def.fields {
            // Find the matching column spec to check if it's localized
            let spec = specs.iter().find(|s| s.col_name == *field_name);
            match spec {
                Some(s) if s.is_localized => {
                    // For localized fields in compound indexes, use default locale column
                    expanded_cols.push(format!("{}__{}", field_name, sanitize_locale(&locale_config.default_locale)));
                }
                _ => {
                    expanded_cols.push(field_name.clone());
                }
            }
        }

        let col_list = expanded_cols.join(", ");
        let name_suffix = index_def.fields.join("_");
        let idx_name = format!("idx_{}_{}", slug, name_suffix);
        let unique = if index_def.unique { "UNIQUE " } else { "" };
        create_stmts.push(format!(
            "CREATE {}INDEX IF NOT EXISTS {} ON {} ({})",
            unique, idx_name, slug, col_list
        ));
        desired.insert(idx_name);
    }

    // 3. Get existing managed indexes (our prefix only)
    let prefix = format!("idx_{}_", slug);
    let mut existing: HashSet<String> = HashSet::new();
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name=?1 AND name LIKE ?2"
    )?;
    let rows = stmt.query_map(rusqlite::params![slug, format!("{}%", prefix)], |row| {
        row.get::<_, String>(0)
    })?;
    for row in rows {
        existing.insert(row?);
    }

    // 4. Drop stale indexes (in existing but not in desired)
    for name in existing.difference(&desired) {
        tracing::info!("Dropping stale index: {}", name);
        conn.execute(&format!("DROP INDEX IF EXISTS {}", name), [])
            .with_context(|| format!("Failed to drop index {}", name))?;
    }

    // 5. Create missing indexes
    for stmt_sql in &create_stmts {
        conn.execute(stmt_sql, [])
            .with_context(|| format!("Failed to create index: {}", stmt_sql))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_helpers::*;
    use super::super::create::create_collection_table;
    use crate::core::collection::*;
    use crate::core::field::{FieldDefinition, FieldType};

    fn get_indexes(conn: &rusqlite::Connection, table: &str) -> HashSet<String> {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type='index' AND tbl_name=?1"
        ).unwrap();
        stmt.query_map([table], |row| row.get::<_, String>(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect()
    }

    #[test]
    fn sync_indexes_creates_index_for_indexed_field() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition::builder("status", FieldType::Text)
                .index(true)
                .build(),
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(indexes.contains("idx_posts_status"), "Should create index for index=true field");
    }

    #[test]
    fn sync_indexes_skips_unique_field() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition::builder("slug", FieldType::Text)
                .unique(true)
                .index(true) // should be skipped because unique=true
                .build(),
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(!indexes.contains("idx_posts_slug"), "Should skip index when unique=true");
    }

    #[test]
    fn sync_indexes_creates_compound_index() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![
            text_field("status"),
            text_field("category"),
        ]);
        def.indexes = vec![IndexDefinition {
            fields: vec!["status".to_string(), "category".to_string()],
            unique: false,
        }];
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(indexes.contains("idx_posts_status_category"), "Should create compound index");
    }

    #[test]
    fn sync_indexes_creates_compound_unique_index() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![
            text_field("category"),
            text_field("slug"),
        ]);
        def.indexes = vec![IndexDefinition {
            fields: vec!["category".to_string(), "slug".to_string()],
            unique: true,
        }];
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(indexes.contains("idx_posts_category_slug"), "Should create compound unique index");
    }

    #[test]
    fn sync_indexes_drops_stale_indexes() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![
            text_field("status"),
            text_field("category"),
        ]);
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
        assert!(!indexes.contains("idx_posts_status"), "Old index should be dropped");
        assert!(indexes.contains("idx_posts_category"), "New index should be created");
    }

    #[test]
    fn sync_indexes_localized_field() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .index(true)
                .build(),
        ]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_indexes(&conn, "posts", &def, &locale_en_de()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(indexes.contains("idx_posts_title__en"), "Should create index per locale: en");
        assert!(indexes.contains("idx_posts_title__de"), "Should create index per locale: de");
    }

    #[test]
    fn sync_indexes_idempotent() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition::builder("status", FieldType::Text)
                .index(true)
                .build(),
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        // Run twice — should not error
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();
        sync_indexes(&conn, "posts", &def, &no_locale()).unwrap();

        let indexes = get_indexes(&conn, "posts");
        assert!(indexes.contains("idx_posts_status"));
    }

    #[test]
    fn sync_indexes_validates_compound_field_names() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.indexes = vec![IndexDefinition {
            fields: vec!["1=1; DROP TABLE posts; --".to_string()],
            unique: false,
        }];
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let result = sync_indexes(&conn, "posts", &def, &no_locale());
        assert!(result.is_err(), "Should reject invalid identifier in compound index");
    }
}
