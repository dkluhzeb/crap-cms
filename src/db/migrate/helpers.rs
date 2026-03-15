//! Shared helpers for migration: table introspection, join tables, versions tables.

use anyhow::{Context as _, Result};
use std::collections::HashSet;

use crate::{
    config::LocaleConfig,
    core::{FieldDefinition, FieldType, field::flatten_array_sub_fields},
    db::DbConnection,
};

pub fn table_exists(conn: &dyn DbConnection, name: &str) -> Result<bool> {
    conn.table_exists(name)
}

pub fn get_table_columns(conn: &dyn DbConnection, table: &str) -> Result<HashSet<String>> {
    conn.get_table_columns(table)
}

pub use crate::db::query::sanitize_locale;

/// Ensure a `_locale` column exists on a junction table (for ALTER TABLE on existing tables).
pub(super) fn ensure_locale_column(
    conn: &dyn DbConnection,
    table_name: &str,
    default_locale: &str,
) -> Result<()> {
    let existing = get_table_columns(conn, table_name)?;

    if !existing.contains("_locale") {
        let sql = format!(
            "ALTER TABLE {} ADD COLUMN _locale TEXT NOT NULL DEFAULT '{}'",
            table_name,
            sanitize_locale(default_locale)
        );
        tracing::info!("Adding _locale column to {}", table_name);
        conn.execute(&sql, &[])
            .with_context(|| format!("Failed to add _locale to {}", table_name))?;
    }
    Ok(())
}

/// Ensure a named column exists on a table (ALTER TABLE ADD COLUMN if missing).
pub(super) fn ensure_column_exists(
    conn: &dyn DbConnection,
    table_name: &str,
    column: &str,
    col_type: &str,
) -> Result<()> {
    let existing = get_table_columns(conn, table_name)?;

    if !existing.contains(column) {
        let sql = format!(
            "ALTER TABLE {} ADD COLUMN {} {}",
            table_name, column, col_type
        );
        tracing::info!("Adding {} column to {}", column, table_name);
        conn.execute(&sql, &[])
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
    pub field: &'a FieldDefinition,
    /// Whether this column is localized (needs per-locale columns)
    pub is_localized: bool,
}

/// Recursively collect column specifications from a field tree.
/// Handles arbitrary nesting of Group, Row, Collapsible, Tabs.
pub(super) fn collect_column_specs<'a>(
    fields: &'a [FieldDefinition],
    locale_config: &LocaleConfig,
) -> Vec<ColumnSpec<'a>> {
    let mut specs = Vec::new();
    collect_column_specs_inner(fields, &mut specs, locale_config, "", false);
    specs
}

fn collect_column_specs_inner<'a>(
    fields: &'a [FieldDefinition],
    specs: &mut Vec<ColumnSpec<'a>>,
    locale_config: &LocaleConfig,
    prefix: &str,
    inherited_localized: bool,
) {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                collect_column_specs_inner(
                    &field.fields,
                    specs,
                    locale_config,
                    &new_prefix,
                    inherited_localized || field.localized,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_column_specs_inner(
                    &field.fields,
                    specs,
                    locale_config,
                    prefix,
                    inherited_localized,
                );
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_column_specs_inner(
                        &tab.fields,
                        specs,
                        locale_config,
                        prefix,
                        inherited_localized,
                    );
                }
            }
            _ => {
                if !field.has_parent_column() {
                    continue;
                }
                let col_name = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                specs.push(ColumnSpec {
                    col_name,
                    field,
                    is_localized: (inherited_localized || field.localized)
                        && locale_config.is_enabled(),
                });
            }
        }
    }
}

/// Sync join tables for has-many relationships and array fields.
pub(super) fn sync_join_tables(
    conn: &dyn DbConnection,
    collection_slug: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    for field in fields {
        let has_locale_col = field.localized && locale_config.is_enabled();

        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                if let Some(ref rc) = field.relationship
                    && rc.has_many
                {
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
                                table_name,
                                collection_slug,
                                poly_col,
                                sanitize_locale(&locale_config.default_locale),
                                poly_pk
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
                        conn.execute(&sql, &[]).with_context(|| {
                            format!("Failed to create junction table {}", table_name)
                        })?;
                    } else {
                        if has_locale_col {
                            ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
                        }
                        // Ensure related_collection column for polymorphic upgrades
                        if rc.is_polymorphic() {
                            ensure_column_exists(
                                conn,
                                &table_name,
                                "related_collection",
                                "TEXT NOT NULL DEFAULT ''",
                            )?;
                        }
                    }
                }
            }
            FieldType::Array => {
                let table_name = format!("{}_{}", collection_slug, field.name);
                let flat_subs = flatten_array_sub_fields(&field.fields);

                if !table_exists(conn, &table_name)? {
                    let mut columns = vec![
                        "id TEXT PRIMARY KEY".to_string(),
                        format!(
                            "parent_id TEXT NOT NULL REFERENCES {}(id) ON DELETE CASCADE",
                            collection_slug
                        ),
                        "_order INTEGER NOT NULL DEFAULT 0".to_string(),
                    ];

                    if has_locale_col {
                        columns.push(format!(
                            "_locale TEXT NOT NULL DEFAULT '{}'",
                            sanitize_locale(&locale_config.default_locale)
                        ));
                    }
                    for sub_field in &flat_subs {
                        columns.push(format!(
                            "{} {}",
                            sub_field.name,
                            conn.column_type_for(&sub_field.field_type)
                        ));
                    }
                    let sql = format!("CREATE TABLE {} ({})", table_name, columns.join(", "));
                    tracing::info!("Creating array table: {}", table_name);
                    conn.execute(&sql, &[])
                        .with_context(|| format!("Failed to create array table {}", table_name))?;
                } else {
                    if has_locale_col {
                        ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
                    }
                    // Alter: add missing sub-field columns
                    let existing = get_table_columns(conn, &table_name)?;
                    for sub_field in &flat_subs {
                        if !existing.contains(&sub_field.name) {
                            let sql = format!(
                                "ALTER TABLE {} ADD COLUMN {} {}",
                                table_name,
                                sub_field.name,
                                conn.column_type_for(&sub_field.field_type)
                            );
                            tracing::info!("Adding column to {}: {}", table_name, sub_field.name);
                            conn.execute(&sql, &[]).with_context(|| {
                                format!("Failed to add column {} to {}", sub_field.name, table_name)
                            })?;
                        }
                    }
                }
            }
            FieldType::Blocks => {
                let table_name = format!("{}_{}", collection_slug, field.name);

                if !table_exists(conn, &table_name)? {
                    let locale_col = if has_locale_col {
                        format!(
                            ", _locale TEXT NOT NULL DEFAULT '{}'",
                            sanitize_locale(&locale_config.default_locale)
                        )
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
                    conn.execute(&sql, &[])
                        .with_context(|| format!("Failed to create blocks table {}", table_name))?;
                } else if has_locale_col {
                    ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                sync_join_tables(conn, collection_slug, &field.fields, locale_config)?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    sync_join_tables(conn, collection_slug, &tab.fields, locale_config)?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Create or verify the `_versions_{slug}` table for document version history.
pub(super) fn sync_versions_table(conn: &dyn DbConnection, slug: &str) -> Result<()> {
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
                created_at {}, \
                updated_at {}\
            )",
            table_name,
            slug,
            conn.timestamp_column_default(),
            conn.timestamp_column_default()
        );
        tracing::info!("Creating versions table: {}", table_name);
        conn.execute(&sql, &[])
            .with_context(|| format!("Failed to create versions table {}", table_name))?;

        // Indexes for efficient version lookups
        conn.execute(
            &format!(
                "CREATE INDEX IF NOT EXISTS idx_{slug}_parent_latest ON {table} (_parent, _latest)",
                slug = slug,
                table = table_name
            ),
            &[],
        )?;
        conn.execute(
            &format!(
                "CREATE INDEX IF NOT EXISTS idx_{slug}_parent_version ON {table} (_parent, _version DESC)",
                slug = slug, table = table_name
            ),
            &[],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CrapConfig, LocaleConfig};
    use crate::core::collection::*;
    use crate::core::field::{FieldDefinition, FieldTab, FieldType, RelationshipConfig};
    use crate::db::{DbConnection, DbPool, pool};
    use tempfile::TempDir;

    fn in_memory_pool() -> (TempDir, DbPool) {
        let dir = TempDir::new().expect("temp dir");
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).expect("in-memory pool");
        (dir, p)
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
        let mut def = CollectionDefinition::new(slug);
        def.fields = fields;
        def
    }

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    // ── table_exists ──────────────────────────────────────────────────────

    #[test]
    fn table_exists_false_initially() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        assert!(!table_exists(&conn, "nonexistent").unwrap());
    }

    #[test]
    fn table_exists_true_after_create() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE test_table (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        assert!(table_exists(&conn, "test_table").unwrap());
    }

    // ── get_table_columns ─────────────────────────────────────────────────

    #[test]
    fn get_table_columns_returns_column_names() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE t (id TEXT, name TEXT, age INTEGER)", &[])
            .unwrap();
        let cols = get_table_columns(&conn, "t").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("name"));
        assert!(cols.contains("age"));
        assert_eq!(cols.len(), 3);
    }

    // ── join tables ───────────────────────────────────────────────────────

    #[test]
    fn has_many_relationship_creates_junction_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("tags", FieldType::Relationship)
                    .relationship(RelationshipConfig::new("tags", true))
                    .build(),
            ],
        );
        // Need parent table first for FK
        super::super::collection::create_collection_table(&conn, "posts", &def, &no_locale())
            .unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(table_exists(&conn, "posts_tags").unwrap());
        let cols = get_table_columns(&conn, "posts_tags").unwrap();
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("related_id"));
        assert!(cols.contains("_order"));
    }

    #[test]
    fn array_field_creates_join_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("items", FieldType::Array)
                    .fields(vec![text_field("name")])
                    .build(),
            ],
        );
        super::super::collection::create_collection_table(&conn, "posts", &def, &no_locale())
            .unwrap();
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
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![FieldDefinition::builder("content", FieldType::Blocks).build()],
        );
        super::super::collection::create_collection_table(&conn, "posts", &def, &no_locale())
            .unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(table_exists(&conn, "posts_content").unwrap());
        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("_block_type"));
        assert!(cols.contains("data"));
    }

    #[test]
    fn blocks_inside_tabs_creates_join_table() {
        // Regression: blocks inside Tabs didn't get their join table created
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let blocks_field = FieldDefinition::builder("content", FieldType::Blocks).build();
        let tabs_field = FieldDefinition::builder("page_settings", FieldType::Tabs)
            .tabs(vec![FieldTab::new("Content", vec![blocks_field])])
            .build();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("title", FieldType::Text).build(),
                tabs_field,
            ],
        );
        super::super::collection::create_collection_table(&conn, "posts", &def, &no_locale())
            .unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_content").unwrap(),
            "blocks table inside Tabs must be created"
        );
        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("_block_type"));
        assert!(cols.contains("data"));
    }

    #[test]
    fn array_inside_row_creates_join_table() {
        // Regression: array inside Row didn't get its join table created
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let array_field = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![text_field("label"), text_field("value")])
            .build();
        let row_field = FieldDefinition::builder("main_row", FieldType::Row)
            .fields(vec![array_field])
            .build();
        let def = simple_collection("posts", vec![text_field("title"), row_field]);
        super::super::collection::create_collection_table(&conn, "posts", &def, &no_locale())
            .unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_items").unwrap(),
            "array table inside Row must be created"
        );
        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("label"));
        assert!(cols.contains("value"));
    }

    #[test]
    fn blocks_inside_collapsible_creates_join_table() {
        // Regression: blocks inside Collapsible didn't get its join table created
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let blocks_field = FieldDefinition::builder("content", FieldType::Blocks).build();
        let collapsible_field = FieldDefinition::builder("advanced", FieldType::Collapsible)
            .fields(vec![blocks_field])
            .build();
        let def = simple_collection("posts", vec![text_field("title"), collapsible_field]);
        super::super::collection::create_collection_table(&conn, "posts", &def, &no_locale())
            .unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_content").unwrap(),
            "blocks table inside Collapsible must be created"
        );
        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("_block_type"));
        assert!(cols.contains("data"));
    }

    // ── localized has-many junction table ───────────────────────────────

    #[test]
    fn localized_has_many_creates_junction_with_locale() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("tags", FieldType::Relationship)
                    .localized(true)
                    .relationship(RelationshipConfig::new("tags", true))
                    .build(),
            ],
        );
        super::super::collection::create_collection_table(&conn, "posts", &def, &locale_en_de())
            .unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(table_exists(&conn, "posts_tags").unwrap());
        let cols = get_table_columns(&conn, "posts_tags").unwrap();
        assert!(
            cols.contains("_locale"),
            "Localized junction table should have _locale column"
        );
    }

    // ── localized array table ───────────────────────────────────────────

    #[test]
    fn localized_array_creates_table_with_locale() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("items", FieldType::Array)
                    .localized(true)
                    .fields(vec![text_field("label")])
                    .build(),
            ],
        );
        super::super::collection::create_collection_table(&conn, "posts", &def, &locale_en_de())
            .unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(table_exists(&conn, "posts_items").unwrap());
        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── localized blocks table ──────────────────────────────────────────

    #[test]
    fn localized_blocks_creates_table_with_locale() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("content", FieldType::Blocks)
                    .localized(true)
                    .build(),
            ],
        );
        super::super::collection::create_collection_table(&conn, "posts", &def, &locale_en_de())
            .unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(table_exists(&conn, "posts_content").unwrap());
        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── ensure_locale_column on existing table ──────────────────────────

    #[test]
    fn ensure_locale_column_adds_to_existing() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute(
            "CREATE TABLE test_join (parent_id TEXT, related_id TEXT)",
            &[],
        )
        .unwrap();

        ensure_locale_column(&conn, "test_join", "en").unwrap();

        let cols = get_table_columns(&conn, "test_join").unwrap();
        assert!(cols.contains("_locale"));

        // Calling again should be a no-op (idempotent)
        ensure_locale_column(&conn, "test_join", "en").unwrap();
    }

    // ── existing localized join table adds _locale via alter ─────────────

    #[test]
    fn existing_has_many_adds_locale_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        // Create parent and junction table without _locale
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute(
            "CREATE TABLE posts_tags (parent_id TEXT, related_id TEXT, _order INTEGER)",
            &[],
        )
        .unwrap();

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("tags", FieldType::Relationship)
                    .localized(true)
                    .relationship(RelationshipConfig::new("tags", true))
                    .build(),
            ],
        );
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_tags").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── existing array table adds sub-field columns and _locale ─────────

    #[test]
    fn existing_array_adds_new_subfield_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute("CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, label TEXT)", &[]).unwrap();

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("items", FieldType::Array)
                    .fields(vec![text_field("label"), text_field("value")])
                    .build(),
            ],
        );
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(
            cols.contains("value"),
            "New sub-field column should be added"
        );
    }

    // ── existing blocks table adds _locale ──────────────────────────────

    #[test]
    fn existing_blocks_adds_locale_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute(
            "CREATE TABLE posts_content (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, _block_type TEXT, data TEXT)",
            &[],
        ).unwrap();

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("content", FieldType::Blocks)
                    .localized(true)
                    .build(),
            ],
        );
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── existing localized array table adds _locale ─────────────────────

    #[test]
    fn existing_array_adds_locale_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute("CREATE TABLE posts_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, label TEXT)", &[]).unwrap();

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("items", FieldType::Array)
                    .localized(true)
                    .fields(vec![text_field("label")])
                    .build(),
            ],
        );
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── collect_column_specs: Group containing layout fields ──────────────

    #[test]
    fn column_specs_group_containing_row() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("r", FieldType::Row)
                        .fields(vec![text_field("title"), text_field("slug")])
                        .build(),
                ])
                .build(),
        ];
        let specs = collect_column_specs(&fields, &no_locale());
        let names: Vec<&str> = specs.iter().map(|s| s.col_name.as_str()).collect();
        assert!(names.contains(&"meta__title"), "Group→Row: meta__title");
        assert!(names.contains(&"meta__slug"), "Group→Row: meta__slug");
    }

    #[test]
    fn column_specs_group_containing_tabs() {
        let fields = vec![
            FieldDefinition::builder("settings", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("t", FieldType::Tabs)
                        .tabs(vec![
                            FieldTab::new("General", vec![text_field("theme")]),
                            FieldTab::new("Advanced", vec![text_field("cache_ttl")]),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let specs = collect_column_specs(&fields, &no_locale());
        let names: Vec<&str> = specs.iter().map(|s| s.col_name.as_str()).collect();
        assert!(
            names.contains(&"settings__theme"),
            "Group→Tabs: settings__theme"
        );
        assert!(
            names.contains(&"settings__cache_ttl"),
            "Group→Tabs: settings__cache_ttl"
        );
    }

    #[test]
    fn column_specs_group_tabs_group_three_levels() {
        let fields = vec![
            FieldDefinition::builder("outer", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("t", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "Tab",
                            vec![
                                FieldDefinition::builder("inner", FieldType::Group)
                                    .fields(vec![text_field("deep")])
                                    .build(),
                            ],
                        )])
                        .build(),
                ])
                .build(),
        ];
        let specs = collect_column_specs(&fields, &no_locale());
        let names: Vec<&str> = specs.iter().map(|s| s.col_name.as_str()).collect();
        assert!(
            names.contains(&"outer__inner__deep"),
            "Group→Tabs→Group: outer__inner__deep"
        );
    }

    #[test]
    fn column_specs_group_containing_localized_tabs() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .localized(true)
                .fields(vec![
                    FieldDefinition::builder("t", FieldType::Tabs)
                        .tabs(vec![FieldTab::new("Content", vec![text_field("title")])])
                        .build(),
                ])
                .build(),
        ];
        let specs = collect_column_specs(&fields, &locale_en_de());
        let names: Vec<&str> = specs.iter().map(|s| s.col_name.as_str()).collect();
        assert!(
            names.contains(&"meta__title"),
            "Localized Group→Tabs: meta__title"
        );
        assert!(
            specs
                .iter()
                .any(|s| s.col_name == "meta__title" && s.is_localized),
            "meta__title should be marked localized via inheritance"
        );
    }

    #[test]
    fn array_with_tabs_creates_flat_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let array_field = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                text_field("plain"),
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![
                        FieldTab::new("General", vec![text_field("title")]),
                        FieldTab::new("Content", vec![text_field("body")]),
                    ])
                    .build(),
            ])
            .build();
        let def = simple_collection("posts", vec![text_field("name"), array_field]);
        super::super::collection::create_collection_table(&conn, "posts", &def, &no_locale())
            .unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(table_exists(&conn, "posts_items").unwrap());
        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("plain"), "plain sub-field column");
        assert!(cols.contains("title"), "title from tabs should be promoted");
        assert!(cols.contains("body"), "body from tabs should be promoted");
        assert!(
            !cols.contains("layout"),
            "layout wrapper should NOT be a column"
        );
    }

    #[test]
    fn array_with_row_creates_flat_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let array_field = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("row_wrap", FieldType::Row)
                    .fields(vec![text_field("x"), text_field("y")])
                    .build(),
            ])
            .build();
        let def = simple_collection("posts", vec![array_field]);
        super::super::collection::create_collection_table(&conn, "posts", &def, &no_locale())
            .unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("x"), "x from row should be promoted");
        assert!(cols.contains("y"), "y from row should be promoted");
        assert!(
            !cols.contains("row_wrap"),
            "row wrapper should NOT be a column"
        );
    }
}
