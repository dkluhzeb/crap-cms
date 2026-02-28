//! Shared helpers for migration: table introspection, join tables, versions tables.

use anyhow::{Context, Result};
use std::collections::HashSet;

use crate::config::LocaleConfig;

pub(super) fn table_exists(conn: &rusqlite::Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub(super) fn get_table_columns(conn: &rusqlite::Connection, table: &str) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table))?;
    let columns: HashSet<String> = stmt.query_map([], |row| {
        row.get::<_, String>(1)
    })?.filter_map(|r| r.ok()).collect();
    Ok(columns)
}

/// Ensure a `_locale` column exists on a junction table (for ALTER TABLE on existing tables).
pub(super) fn ensure_locale_column(conn: &rusqlite::Connection, table_name: &str, default_locale: &str) -> Result<()> {
    let existing = get_table_columns(conn, table_name)?;
    if !existing.contains("_locale") {
        let sql = format!(
            "ALTER TABLE {} ADD COLUMN _locale TEXT NOT NULL DEFAULT '{}'",
            table_name, default_locale
        );
        tracing::info!("Adding _locale column to {}", table_name);
        conn.execute(&sql, [])
            .with_context(|| format!("Failed to add _locale to {}", table_name))?;
    }
    Ok(())
}

/// Sync join tables for has-many relationships and array fields.
pub(super) fn sync_join_tables(
    conn: &rusqlite::Connection,
    collection_slug: &str,
    fields: &[crate::core::field::FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    use crate::core::field::FieldType;

    for field in fields {
        let has_locale_col = field.localized && locale_config.is_enabled();

        match field.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        let table_name = format!("{}_{}", collection_slug, field.name);
                        if !table_exists(conn, &table_name)? {
                            let sql = if has_locale_col {
                                format!(
                                    "CREATE TABLE {} (\
                                        parent_id TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE, \
                                        related_id TEXT NOT NULL, \
                                        _order INTEGER NOT NULL DEFAULT 0, \
                                        _locale TEXT NOT NULL DEFAULT '{}', \
                                        PRIMARY KEY (parent_id, related_id, _locale)\
                                    )",
                                    table_name, collection_slug, locale_config.default_locale
                                )
                            } else {
                                format!(
                                    "CREATE TABLE {} (\
                                        parent_id TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE, \
                                        related_id TEXT NOT NULL, \
                                        _order INTEGER NOT NULL DEFAULT 0, \
                                        PRIMARY KEY (parent_id, related_id)\
                                    )",
                                    table_name, collection_slug
                                )
                            };
                            tracing::info!("Creating junction table: {}", table_name);
                            conn.execute(&sql, [])
                                .with_context(|| format!("Failed to create junction table {}", table_name))?;
                        } else if has_locale_col {
                            ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
                        }
                    }
                }
            }
            FieldType::Array => {
                let table_name = format!("{}_{}", collection_slug, field.name);
                if !table_exists(conn, &table_name)? {
                    let mut columns = vec![
                        "id TEXT PRIMARY KEY".to_string(),
                        format!("parent_id TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE", collection_slug),
                        "_order INTEGER NOT NULL DEFAULT 0".to_string(),
                    ];
                    if has_locale_col {
                        columns.push(format!("_locale TEXT NOT NULL DEFAULT '{}'", locale_config.default_locale));
                    }
                    for sub_field in &field.fields {
                        columns.push(format!("{} {}", sub_field.name, sub_field.field_type.sqlite_type()));
                    }
                    let sql = format!(
                        "CREATE TABLE {} ({})",
                        table_name,
                        columns.join(", ")
                    );
                    tracing::info!("Creating array table: {}", table_name);
                    conn.execute(&sql, [])
                        .with_context(|| format!("Failed to create array table {}", table_name))?;
                } else {
                    if has_locale_col {
                        ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
                    }
                    // Alter: add missing sub-field columns
                    let existing = get_table_columns(conn, &table_name)?;
                    for sub_field in &field.fields {
                        if !existing.contains(&sub_field.name) {
                            let sql = format!(
                                "ALTER TABLE {} ADD COLUMN {} {}",
                                table_name, sub_field.name, sub_field.field_type.sqlite_type()
                            );
                            tracing::info!("Adding column to {}: {}", table_name, sub_field.name);
                            conn.execute(&sql, [])
                                .with_context(|| format!("Failed to add column {} to {}", sub_field.name, table_name))?;
                        }
                    }
                }
            }
            FieldType::Blocks => {
                let table_name = format!("{}_{}", collection_slug, field.name);
                if !table_exists(conn, &table_name)? {
                    let locale_col = if has_locale_col {
                        format!(", _locale TEXT NOT NULL DEFAULT '{}'", locale_config.default_locale)
                    } else {
                        String::new()
                    };
                    let sql = format!(
                        "CREATE TABLE {} (\
                            id TEXT PRIMARY KEY, \
                            parent_id TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE, \
                            _order INTEGER NOT NULL DEFAULT 0, \
                            _block_type TEXT NOT NULL, \
                            data TEXT NOT NULL DEFAULT '{{}}'\
                            {}\
                        )",
                        table_name, collection_slug, locale_col
                    );
                    tracing::info!("Creating blocks table: {}", table_name);
                    conn.execute(&sql, [])
                        .with_context(|| format!("Failed to create blocks table {}", table_name))?;
                } else if has_locale_col {
                    ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Create or verify the `_versions_{slug}` table for document version history.
pub(super) fn sync_versions_table(conn: &rusqlite::Connection, slug: &str) -> Result<()> {
    let table_name = format!("_versions_{}", slug);
    if !table_exists(conn, &table_name)? {
        let sql = format!(
            "CREATE TABLE {} (\
                id TEXT PRIMARY KEY, \
                _parent TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE, \
                _version INTEGER NOT NULL, \
                _status TEXT NOT NULL, \
                _latest INTEGER NOT NULL DEFAULT 0, \
                snapshot TEXT NOT NULL, \
                created_at TEXT DEFAULT (datetime('now')), \
                updated_at TEXT DEFAULT (datetime('now'))\
            )",
            table_name, slug
        );
        tracing::info!("Creating versions table: {}", table_name);
        conn.execute(&sql, [])
            .with_context(|| format!("Failed to create versions table {}", table_name))?;

        // Indexes for efficient version lookups
        conn.execute(
            &format!(
                "CREATE INDEX IF NOT EXISTS idx_{slug}_parent_latest ON {table} (_parent, _latest)",
                slug = slug, table = table_name
            ),
            [],
        )?;
        conn.execute(
            &format!(
                "CREATE INDEX IF NOT EXISTS idx_{slug}_parent_version ON {table} (_parent, _version DESC)",
                slug = slug, table = table_name
            ),
            [],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocaleConfig;
    use crate::core::collection::*;
    use crate::core::field::{FieldDefinition, FieldType, RelationshipConfig};
    use crate::db::DbPool;

    fn in_memory_pool() -> DbPool {
        let manager = r2d2_sqlite::SqliteConnectionManager::memory()
            .with_flags(rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_FULL_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_SHARED_CACHE);
        r2d2::Pool::builder()
            .max_size(2)
            .build(manager)
            .expect("in-memory pool")
    }

    fn no_locale() -> LocaleConfig {
        LocaleConfig::default()
    }

    fn locale_en_de() -> LocaleConfig {
        LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        }
    }

    fn simple_collection(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
        CollectionDefinition {
            slug: slug.to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields,
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            live: None,
            versions: None,
        }
    }

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Text,
            ..Default::default()
        }
    }

    // ── table_exists ──────────────────────────────────────────────────────

    #[test]
    fn table_exists_false_initially() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        assert!(!table_exists(&conn, "nonexistent").unwrap());
    }

    #[test]
    fn table_exists_true_after_create() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE test_table (id TEXT PRIMARY KEY)", []).unwrap();
        assert!(table_exists(&conn, "test_table").unwrap());
    }

    // ── get_table_columns ─────────────────────────────────────────────────

    #[test]
    fn get_table_columns_returns_column_names() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE t (id TEXT, name TEXT, age INTEGER)", []).unwrap();
        let cols = get_table_columns(&conn, "t").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("name"));
        assert!(cols.contains("age"));
        assert_eq!(cols.len(), 3);
    }

    // ── join tables ───────────────────────────────────────────────────────

    #[test]
    fn has_many_relationship_creates_junction_table() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "tags".to_string(),
                field_type: FieldType::Relationship,
                relationship: Some(RelationshipConfig {
                    collection: "tags".to_string(),
                    has_many: true,
                    max_depth: None,
                }),
                ..Default::default()
            },
        ]);
        // Need parent table first for FK
        super::super::collection::create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(table_exists(&conn, "posts_tags").unwrap());
        let cols = get_table_columns(&conn, "posts_tags").unwrap();
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("related_id"));
        assert!(cols.contains("_order"));
    }

    #[test]
    fn array_field_creates_join_table() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "items".to_string(),
                field_type: FieldType::Array,
                fields: vec![text_field("name")],
                ..Default::default()
            },
        ]);
        super::super::collection::create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(table_exists(&conn, "posts_items").unwrap());
        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("_order"));
        assert!(cols.contains("name"));
    }

    #[test]
    fn blocks_field_creates_join_table() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Blocks,
                ..Default::default()
            },
        ]);
        super::super::collection::create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(table_exists(&conn, "posts_content").unwrap());
        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("_block_type"));
        assert!(cols.contains("data"));
    }

    // ── localized has-many junction table ───────────────────────────────

    #[test]
    fn localized_has_many_creates_junction_with_locale() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "tags".to_string(),
                field_type: FieldType::Relationship,
                localized: true,
                relationship: Some(RelationshipConfig {
                    collection: "tags".to_string(),
                    has_many: true,
                    max_depth: None,
                }),
                ..Default::default()
            },
        ]);
        super::super::collection::create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(table_exists(&conn, "posts_tags").unwrap());
        let cols = get_table_columns(&conn, "posts_tags").unwrap();
        assert!(cols.contains("_locale"), "Localized junction table should have _locale column");
    }

    // ── localized array table ───────────────────────────────────────────

    #[test]
    fn localized_array_creates_table_with_locale() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "items".to_string(),
                field_type: FieldType::Array,
                localized: true,
                fields: vec![text_field("label")],
                ..Default::default()
            },
        ]);
        super::super::collection::create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(table_exists(&conn, "posts_items").unwrap());
        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── localized blocks table ──────────────────────────────────────────

    #[test]
    fn localized_blocks_creates_table_with_locale() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Blocks,
                localized: true,
                ..Default::default()
            },
        ]);
        super::super::collection::create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(table_exists(&conn, "posts_content").unwrap());
        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── ensure_locale_column on existing table ──────────────────────────

    #[test]
    fn ensure_locale_column_adds_to_existing() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE test_join (parent_id TEXT, related_id TEXT)", []).unwrap();

        ensure_locale_column(&conn, "test_join", "en").unwrap();

        let cols = get_table_columns(&conn, "test_join").unwrap();
        assert!(cols.contains("_locale"));

        // Calling again should be a no-op (idempotent)
        ensure_locale_column(&conn, "test_join", "en").unwrap();
    }

    // ── existing localized join table adds _locale via alter ─────────────

    #[test]
    fn existing_has_many_adds_locale_column() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        // Create parent and junction table without _locale
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", []).unwrap();
        conn.execute("CREATE TABLE posts_tags (parent_id TEXT, related_id TEXT, _order INTEGER)", []).unwrap();

        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "tags".to_string(),
                field_type: FieldType::Relationship,
                localized: true,
                relationship: Some(RelationshipConfig {
                    collection: "tags".to_string(),
                    has_many: true,
                    max_depth: None,
                }),
                ..Default::default()
            },
        ]);
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_tags").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── existing array table adds sub-field columns and _locale ─────────

    #[test]
    fn existing_array_adds_new_subfield_columns() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", []).unwrap();
        conn.execute("CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, label TEXT)", []).unwrap();

        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "items".to_string(),
                field_type: FieldType::Array,
                fields: vec![text_field("label"), text_field("value")],
                ..Default::default()
            },
        ]);
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("value"), "New sub-field column should be added");
    }

    // ── existing blocks table adds _locale ──────────────────────────────

    #[test]
    fn existing_blocks_adds_locale_column() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", []).unwrap();
        conn.execute(
            "CREATE TABLE posts_content (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, _block_type TEXT, data TEXT)",
            [],
        ).unwrap();

        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "content".to_string(),
                field_type: FieldType::Blocks,
                localized: true,
                ..Default::default()
            },
        ]);
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── existing localized array table adds _locale ─────────────────────

    #[test]
    fn existing_array_adds_locale_column() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", []).unwrap();
        conn.execute("CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, label TEXT)", []).unwrap();

        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "items".to_string(),
                field_type: FieldType::Array,
                localized: true,
                fields: vec![text_field("label")],
                ..Default::default()
            },
        ]);
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("_locale"));
    }
}
