//! Shared helpers for migration: table introspection, join tables, versions tables.

use anyhow::{Context, Result};
use std::collections::HashSet;

use crate::config::LocaleConfig;

pub fn table_exists(conn: &rusqlite::Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub fn get_table_columns(conn: &rusqlite::Connection, table: &str) -> Result<HashSet<String>> {
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

/// Ensure a named column exists on a table (ALTER TABLE ADD COLUMN if missing).
pub(super) fn ensure_column_exists(conn: &rusqlite::Connection, table_name: &str, column: &str, col_type: &str) -> Result<()> {
    let existing = get_table_columns(conn, table_name)?;
    if !existing.contains(column) {
        let sql = format!("ALTER TABLE {} ADD COLUMN {} {}", table_name, column, col_type);
        tracing::info!("Adding {} column to {}", column, table_name);
        conn.execute(&sql, [])
            .with_context(|| format!("Failed to add {} to {}", column, table_name))?;
    }
    Ok(())
}

/// A column specification derived from a field definition.
/// Used by migration code to generate CREATE TABLE / ALTER TABLE statements.
pub(super) struct ColumnSpec<'a> {
    /// The column name (e.g., "title", "social__github")
    pub col_name: String,
    /// The field definition this column comes from (used for type, constraints)
    pub field: &'a crate::core::field::FieldDefinition,
    /// Whether this column is localized (needs per-locale columns)
    pub is_localized: bool,
}

/// Recursively collect column specifications from a field tree.
/// Handles arbitrary nesting of Group, Row, Collapsible, Tabs.
pub(super) fn collect_column_specs<'a>(
    fields: &'a [crate::core::field::FieldDefinition],
    locale_config: &LocaleConfig,
) -> Vec<ColumnSpec<'a>> {
    let mut specs = Vec::new();
    collect_column_specs_inner(fields, &mut specs, locale_config, "", false);
    specs
}

fn collect_column_specs_inner<'a>(
    fields: &'a [crate::core::field::FieldDefinition],
    specs: &mut Vec<ColumnSpec<'a>>,
    locale_config: &LocaleConfig,
    prefix: &str,
    inherited_localized: bool,
) {
    use crate::core::field::FieldType;

    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                collect_column_specs_inner(
                    &field.fields, specs, locale_config,
                    &new_prefix, inherited_localized || field.localized,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_column_specs_inner(&field.fields, specs, locale_config, prefix, inherited_localized);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_column_specs_inner(&tab.fields, specs, locale_config, prefix, inherited_localized);
                }
            }
            _ => {
                if !field.has_parent_column() { continue; }
                let col_name = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                specs.push(ColumnSpec {
                    col_name,
                    field,
                    is_localized: (inherited_localized || field.localized) && locale_config.is_enabled(),
                });
            }
        }
    }
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
            FieldType::Relationship | FieldType::Upload => {
                if let Some(ref rc) = field.relationship {
                    if rc.has_many {
                        let table_name = format!("{}_{}", collection_slug, field.name);
                        if !table_exists(conn, &table_name)? {
                            let poly_col = if rc.is_polymorphic() {
                                "related_collection TEXT NOT NULL DEFAULT '', "
                            } else {
                                ""
                            };
                            let poly_pk = if rc.is_polymorphic() {
                                ", related_collection"
                            } else {
                                ""
                            };
                            let sql = if has_locale_col {
                                format!(
                                    "CREATE TABLE {} (\
                                        parent_id TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE, \
                                        related_id TEXT NOT NULL, \
                                        {}\
                                        _order INTEGER NOT NULL DEFAULT 0, \
                                        _locale TEXT NOT NULL DEFAULT '{}', \
                                        PRIMARY KEY (parent_id, related_id{}, _locale)\
                                    )",
                                    table_name, collection_slug, poly_col, locale_config.default_locale, poly_pk
                                )
                            } else {
                                format!(
                                    "CREATE TABLE {} (\
                                        parent_id TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE, \
                                        related_id TEXT NOT NULL, \
                                        {}\
                                        _order INTEGER NOT NULL DEFAULT 0, \
                                        PRIMARY KEY (parent_id, related_id{})\
                                    )",
                                    table_name, collection_slug, poly_col, poly_pk
                                )
                            };
                            tracing::info!("Creating junction table: {}", table_name);
                            conn.execute(&sql, [])
                                .with_context(|| format!("Failed to create junction table {}", table_name))?;
                        } else {
                            if has_locale_col {
                                ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
                            }
                            // Ensure related_collection column for polymorphic upgrades
                            if rc.is_polymorphic() {
                                ensure_column_exists(conn, &table_name, "related_collection", "TEXT NOT NULL DEFAULT ''")?;
                            }
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
                    polymorphic: vec![],
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
                    polymorphic: vec![],
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
                    polymorphic: vec![],
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

    // ── collect_column_specs: Group containing layout fields ──────────────

    #[test]
    fn column_specs_group_containing_row() {
        let fields = vec![
            FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "r".to_string(),
                        field_type: FieldType::Row,
                        fields: vec![text_field("title"), text_field("slug")],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ];
        let specs = collect_column_specs(&fields, &no_locale());
        let names: Vec<&str> = specs.iter().map(|s| s.col_name.as_str()).collect();
        assert!(names.contains(&"meta__title"), "Group→Row: meta__title");
        assert!(names.contains(&"meta__slug"), "Group→Row: meta__slug");
    }

    #[test]
    fn column_specs_group_containing_tabs() {
        use crate::core::field::FieldTab;
        let fields = vec![
            FieldDefinition {
                name: "settings".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "t".to_string(),
                        field_type: FieldType::Tabs,
                        tabs: vec![
                            FieldTab {
                                label: "General".to_string(),
                                description: None,
                                fields: vec![text_field("theme")],
                            },
                            FieldTab {
                                label: "Advanced".to_string(),
                                description: None,
                                fields: vec![text_field("cache_ttl")],
                            },
                        ],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ];
        let specs = collect_column_specs(&fields, &no_locale());
        let names: Vec<&str> = specs.iter().map(|s| s.col_name.as_str()).collect();
        assert!(names.contains(&"settings__theme"), "Group→Tabs: settings__theme");
        assert!(names.contains(&"settings__cache_ttl"), "Group→Tabs: settings__cache_ttl");
    }

    #[test]
    fn column_specs_group_tabs_group_three_levels() {
        use crate::core::field::FieldTab;
        let fields = vec![
            FieldDefinition {
                name: "outer".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "t".to_string(),
                        field_type: FieldType::Tabs,
                        tabs: vec![FieldTab {
                            label: "Tab".to_string(),
                            description: None,
                            fields: vec![
                                FieldDefinition {
                                    name: "inner".to_string(),
                                    field_type: FieldType::Group,
                                    fields: vec![text_field("deep")],
                                    ..Default::default()
                                },
                            ],
                        }],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ];
        let specs = collect_column_specs(&fields, &no_locale());
        let names: Vec<&str> = specs.iter().map(|s| s.col_name.as_str()).collect();
        assert!(names.contains(&"outer__inner__deep"), "Group→Tabs→Group: outer__inner__deep");
    }

    #[test]
    fn column_specs_group_containing_localized_tabs() {
        use crate::core::field::FieldTab;
        let fields = vec![
            FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                localized: true,
                fields: vec![
                    FieldDefinition {
                        name: "t".to_string(),
                        field_type: FieldType::Tabs,
                        tabs: vec![FieldTab {
                            label: "Content".to_string(),
                            description: None,
                            fields: vec![text_field("title")],
                        }],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ];
        let specs = collect_column_specs(&fields, &locale_en_de());
        let names: Vec<&str> = specs.iter().map(|s| s.col_name.as_str()).collect();
        assert!(names.contains(&"meta__title"), "Localized Group→Tabs: meta__title");
        assert!(specs.iter().any(|s| s.col_name == "meta__title" && s.is_localized),
                "meta__title should be marked localized via inheritance");
    }
}
