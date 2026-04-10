//! Join table sync for has-many relationships, array fields, and blocks.

use anyhow::{Context as _, Result};
use tracing::info;

use crate::{
    config::LocaleConfig,
    core::{FieldDefinition, FieldType, field::flatten_array_sub_fields},
    db::{
        DbConnection,
        query::helpers::{join_table, prefixed_name, tz_column},
    },
};

use super::{
    column_specs::ensure_locale_column,
    introspection::{get_table_columns, sanitize_locale, table_exists},
};

/// Sync join tables for has-many relationships and array fields.
pub(in crate::db::migrate) fn sync_join_tables(
    conn: &dyn DbConnection,
    collection_slug: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
) -> Result<()> {
    sync_join_tables_inner(conn, collection_slug, fields, locale_config, "", false)
}

fn sync_join_tables_inner(
    conn: &dyn DbConnection,
    collection_slug: &str,
    fields: &[FieldDefinition],
    locale_config: &LocaleConfig,
    prefix: &str,
    inherited_localized: bool,
) -> Result<()> {
    for field in fields {
        let has_locale_col = (inherited_localized || field.localized) && locale_config.is_enabled();
        let full_name = prefixed_name(prefix, &field.name);

        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                sync_relationship_table(
                    conn,
                    collection_slug,
                    field,
                    &full_name,
                    has_locale_col,
                    locale_config,
                )?;
            }
            FieldType::Array => {
                sync_array_table(
                    conn,
                    collection_slug,
                    field,
                    &full_name,
                    has_locale_col,
                    locale_config,
                )?;
            }
            FieldType::Blocks => {
                sync_blocks_table(
                    conn,
                    collection_slug,
                    &full_name,
                    has_locale_col,
                    locale_config,
                )?;
            }
            FieldType::Group => {
                sync_join_tables_inner(
                    conn,
                    collection_slug,
                    &field.fields,
                    locale_config,
                    &full_name,
                    inherited_localized || field.localized,
                )?;
            }
            FieldType::Row | FieldType::Collapsible => {
                sync_join_tables_inner(
                    conn,
                    collection_slug,
                    &field.fields,
                    locale_config,
                    prefix,
                    inherited_localized,
                )?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    sync_join_tables_inner(
                        conn,
                        collection_slug,
                        &tab.fields,
                        locale_config,
                        prefix,
                        inherited_localized,
                    )?;
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Sync a has-many relationship junction table.
fn sync_relationship_table(
    conn: &dyn DbConnection,
    collection_slug: &str,
    field: &FieldDefinition,
    full_name: &str,
    has_locale_col: bool,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let rc = match &field.relationship {
        Some(rc) if rc.has_many => rc,
        _ => return Ok(()),
    };

    let table_name = join_table(collection_slug, full_name);

    if !table_exists(conn, &table_name)? {
        create_junction_table(
            conn,
            &table_name,
            collection_slug,
            rc.is_polymorphic(),
            has_locale_col,
            locale_config,
        )?;
    } else {
        if has_locale_col {
            ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
        }

        if rc.is_polymorphic() {
            let cols = get_table_columns(conn, &table_name)?;

            if !cols.contains("related_collection") {
                rebuild_junction_table_for_polymorphic(
                    conn,
                    &table_name,
                    collection_slug,
                    has_locale_col,
                )?;
            }
        }
    }

    Ok(())
}

/// Create a new junction table for has-many relationships.
fn create_junction_table(
    conn: &dyn DbConnection,
    table_name: &str,
    collection_slug: &str,
    is_polymorphic: bool,
    has_locale_col: bool,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let poly_col = if is_polymorphic {
        "related_collection TEXT NOT NULL DEFAULT '', "
    } else {
        ""
    };
    let poly_pk = if is_polymorphic {
        ", related_collection"
    } else {
        ""
    };

    let sql = if has_locale_col {
        format!(
            "CREATE TABLE \"{}\" (\
                parent_id TEXT NOT NULL REFERENCES \"{}\"(id) ON DELETE CASCADE, \
                related_id TEXT NOT NULL, \
                {}\
                _order INTEGER NOT NULL DEFAULT 0, \
                _locale TEXT NOT NULL DEFAULT '{}', \
                PRIMARY KEY (parent_id, related_id{}, _locale)\
            )",
            table_name,
            collection_slug,
            poly_col,
            sanitize_locale(&locale_config.default_locale)?,
            poly_pk
        )
    } else {
        format!(
            "CREATE TABLE \"{}\" (\
                parent_id TEXT NOT NULL REFERENCES \"{}\"(id) ON DELETE CASCADE, \
                related_id TEXT NOT NULL, \
                {}\
                _order INTEGER NOT NULL DEFAULT 0, \
                PRIMARY KEY (parent_id, related_id{})\
            )",
            table_name, collection_slug, poly_col, poly_pk
        )
    };

    info!("Creating junction table: {}", table_name);
    conn.execute_ddl(&sql, &[])
        .with_context(|| format!("Failed to create junction table {}", table_name))?;

    Ok(())
}

/// Sync an array join table (create or alter).
fn sync_array_table(
    conn: &dyn DbConnection,
    collection_slug: &str,
    field: &FieldDefinition,
    full_name: &str,
    has_locale_col: bool,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let table_name = join_table(collection_slug, full_name);
    let flat_subs = flatten_array_sub_fields(&field.fields);

    if !table_exists(conn, &table_name)? {
        create_array_table(
            conn,
            &table_name,
            collection_slug,
            &flat_subs,
            has_locale_col,
            locale_config,
        )?;
    } else {
        if has_locale_col {
            ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
        }

        alter_array_table(conn, &table_name, &flat_subs)?;
    }

    Ok(())
}

/// Create a new array join table.
fn create_array_table(
    conn: &dyn DbConnection,
    table_name: &str,
    collection_slug: &str,
    flat_subs: &[&FieldDefinition],
    has_locale_col: bool,
    locale_config: &LocaleConfig,
) -> Result<()> {
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
            sanitize_locale(&locale_config.default_locale)?
        ));
    }

    for sub_field in flat_subs {
        columns.push(format!(
            "{} {}",
            sub_field.name,
            conn.column_type_for(&sub_field.field_type)
        ));

        if sub_field.field_type == FieldType::Date && sub_field.timezone {
            columns.push(format!("{}_tz TEXT", sub_field.name));
        }
    }

    let sql = format!("CREATE TABLE \"{}\" ({})", table_name, columns.join(", "));

    info!("Creating array table: {}", table_name);
    conn.execute_ddl(&sql, &[])
        .with_context(|| format!("Failed to create array table {}", table_name))?;

    Ok(())
}

/// Add missing sub-field columns to an existing array table.
fn alter_array_table(
    conn: &dyn DbConnection,
    table_name: &str,
    flat_subs: &[&FieldDefinition],
) -> Result<()> {
    let existing = get_table_columns(conn, table_name)?;

    for sub_field in flat_subs {
        if !existing.contains(&sub_field.name) {
            let sql = format!(
                "ALTER TABLE \"{}\" ADD COLUMN \"{}\" {}",
                table_name,
                sub_field.name,
                conn.column_type_for(&sub_field.field_type)
            );
            info!("Adding column to {}: {}", table_name, sub_field.name);
            conn.execute_ddl(&sql, &[]).with_context(|| {
                format!("Failed to add column {} to {}", sub_field.name, table_name)
            })?;
        }

        if sub_field.field_type == FieldType::Date && sub_field.timezone {
            let tz_col = tz_column(&sub_field.name);

            if !existing.contains(&tz_col) {
                let sql = format!(
                    "ALTER TABLE \"{}\" ADD COLUMN \"{}\" TEXT",
                    table_name, tz_col
                );
                info!("Adding column to {}: {}", table_name, tz_col);
                conn.execute_ddl(&sql, &[]).with_context(|| {
                    format!("Failed to add column {} to {}", tz_col, table_name)
                })?;
            }
        }
    }

    Ok(())
}

/// Sync a blocks join table (create or ensure locale column).
fn sync_blocks_table(
    conn: &dyn DbConnection,
    collection_slug: &str,
    full_name: &str,
    has_locale_col: bool,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let table_name = join_table(collection_slug, full_name);

    if !table_exists(conn, &table_name)? {
        let locale_col = if has_locale_col {
            let default_loc = sanitize_locale(&locale_config.default_locale)?;
            format!(", _locale TEXT NOT NULL DEFAULT '{}'", default_loc)
        } else {
            String::new()
        };

        let sql = format!(
            "CREATE TABLE \"{}\" (\
                id TEXT PRIMARY KEY, \
                parent_id TEXT NOT NULL REFERENCES \"{}\"(id) ON DELETE CASCADE, \
                _order INTEGER NOT NULL DEFAULT 0, \
                _block_type TEXT NOT NULL, \
                data TEXT NOT NULL DEFAULT '{{}}'\
                {}\
            )",
            table_name, collection_slug, locale_col
        );

        info!("Creating blocks table: {}", table_name);
        conn.execute_ddl(&sql, &[])
            .with_context(|| format!("Failed to create blocks table {}", table_name))?;
    } else if has_locale_col {
        ensure_locale_column(conn, &table_name, &locale_config.default_locale)?;
    }

    Ok(())
}

/// Rebuild a junction table to add `related_collection` column with correct PRIMARY KEY.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn rebuild_junction_table_for_polymorphic(
    conn: &dyn DbConnection,
    table_name: &str,
    collection_slug: &str,
    has_locale: bool,
) -> Result<()> {
    let temp = format!("_{}_migrate", table_name);

    conn.execute_batch_ddl(&format!(
        "ALTER TABLE \"{}\" RENAME TO \"{}\"",
        table_name, temp
    ))?;

    let locale_col = if has_locale { ", _locale TEXT" } else { "" };
    let locale_pk = if has_locale { ", _locale" } else { "" };

    conn.execute_batch_ddl(&format!(
        "CREATE TABLE \"{}\" (\
            parent_id TEXT NOT NULL REFERENCES \"{}\"(id) ON DELETE CASCADE, \
            related_id TEXT NOT NULL, \
            related_collection TEXT NOT NULL DEFAULT '', \
            _order INTEGER NOT NULL DEFAULT 0{}, \
            PRIMARY KEY (parent_id, related_id, related_collection{})\
        )",
        table_name, collection_slug, locale_col, locale_pk
    ))?;

    if has_locale {
        conn.execute_batch(&format!(
            "INSERT INTO \"{}\" (parent_id, related_id, related_collection, _order, _locale) \
             SELECT parent_id, related_id, '' AS related_collection, _order, _locale FROM \"{}\"",
            table_name, temp
        ))?;
    } else {
        conn.execute_batch(&format!(
            "INSERT INTO \"{}\" (parent_id, related_id, related_collection, _order) \
             SELECT parent_id, related_id, '' AS related_collection, _order FROM \"{}\"",
            table_name, temp
        ))?;
    }

    conn.execute_batch_ddl(&format!("DROP TABLE \"{}\"", temp))?;

    info!(
        "Rebuilt junction table {} for polymorphic upgrade (updated PRIMARY KEY)",
        table_name
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldDefinition, FieldTab, FieldType, RelationshipConfig};
    use crate::db::DbValue;
    use crate::db::migrate::collection::{create_collection_table, test_helpers::*};
    use crate::db::migrate::helpers::get_table_columns;

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
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
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
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
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
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
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
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
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
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
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
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
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
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
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
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
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
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(table_exists(&conn, "posts_content").unwrap());
        let cols = get_table_columns(&conn, "posts_content").unwrap();
        assert!(cols.contains("_locale"));
    }

    // ── ensure_locale_column on existing table ──────────────────────────

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
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
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

    // ── Group > Array/Blocks prefixed join tables ─────────────────────────

    #[test]
    fn group_array_creates_prefixed_join_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("config", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("items", FieldType::Array)
                            .fields(vec![
                                text_field("name"),
                                FieldDefinition::builder("score", FieldType::Number).build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_config__items").unwrap(),
            "Group > Array should create prefixed join table posts_config__items"
        );
        let cols = get_table_columns(&conn, "posts_config__items").unwrap();
        assert!(cols.contains("name"), "should have name column");
        assert!(cols.contains("score"), "should have score column");
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("_order"));
    }

    #[test]
    fn group_blocks_creates_prefixed_join_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("config", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("content", FieldType::Blocks).build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_config__content").unwrap(),
            "Group > Blocks should create prefixed join table posts_config__content"
        );
        let cols = get_table_columns(&conn, "posts_config__content").unwrap();
        assert!(
            cols.contains("_block_type"),
            "should have _block_type column"
        );
        assert!(cols.contains("data"), "should have data column");
    }

    #[test]
    fn group_array_localized_creates_table_with_locale() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("config", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("items", FieldType::Array)
                            .localized(true)
                            .fields(vec![text_field("label")])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        assert!(
            table_exists(&conn, "posts_config__items").unwrap(),
            "Group > localized Array should create prefixed join table"
        );
        let cols = get_table_columns(&conn, "posts_config__items").unwrap();
        assert!(
            cols.contains("_locale"),
            "localized Array inside Group should have _locale column"
        );
        assert!(cols.contains("label"));
    }

    #[test]
    fn group_relationship_creates_prefixed_junction_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("config", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("tags", FieldType::Relationship)
                            .relationship(RelationshipConfig::new("tags", true))
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_config__tags").unwrap(),
            "Group > Relationship should create prefixed junction table"
        );
        let cols = get_table_columns(&conn, "posts_config__tags").unwrap();
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("related_id"));
        assert!(cols.contains("_order"));
    }

    #[test]
    fn group_group_array_creates_deeply_prefixed_join_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("outer", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("inner", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("items", FieldType::Array)
                                    .fields(vec![text_field("name")])
                                    .build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_outer__inner__items").unwrap(),
            "Group > Group > Array should create double-prefixed join table"
        );
        let cols = get_table_columns(&conn, "posts_outer__inner__items").unwrap();
        assert!(cols.contains("name"));
        assert!(cols.contains("parent_id"));
        assert!(cols.contains("_order"));
    }

    #[test]
    fn group_group_blocks_creates_deeply_prefixed_join_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("outer", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("inner", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("content", FieldType::Blocks).build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        assert!(
            table_exists(&conn, "posts_outer__inner__content").unwrap(),
            "Group > Group > Blocks should create double-prefixed join table"
        );
        let cols = get_table_columns(&conn, "posts_outer__inner__content").unwrap();
        assert!(cols.contains("_block_type"));
        assert!(cols.contains("data"));
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
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts_items").unwrap();
        assert!(cols.contains("x"), "x from row should be promoted");
        assert!(cols.contains("y"), "y from row should be promoted");
        assert!(
            !cols.contains("row_wrap"),
            "row wrapper should NOT be a column"
        );
    }

    // ── array timezone companion columns ──────────────────────────────────

    #[test]
    fn array_date_with_timezone_creates_tz_column() {
        // Regression: array sub-fields with timezone Date didn't get _tz companion column
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let array_field = FieldDefinition::builder("events", FieldType::Array)
            .fields(vec![
                text_field("title"),
                FieldDefinition::builder("scheduled_at", FieldType::Date)
                    .timezone(true)
                    .build(),
            ])
            .build();
        let def = simple_collection("posts", vec![array_field]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts_events").unwrap();
        assert!(cols.contains("scheduled_at"), "date column should exist");
        assert!(
            cols.contains("scheduled_at_tz"),
            "timezone companion column should exist for Date+timezone in array"
        );
    }

    #[test]
    fn existing_array_adds_tz_column_on_alter() {
        // Regression: ALTER path also missed _tz companion columns
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute(
        "CREATE TABLE posts_events (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, title TEXT)",
        &[],
    )
    .unwrap();

        let array_field = FieldDefinition::builder("events", FieldType::Array)
            .fields(vec![
                text_field("title"),
                FieldDefinition::builder("scheduled_at", FieldType::Date)
                    .timezone(true)
                    .build(),
            ])
            .build();
        let def = simple_collection("posts", vec![array_field]);
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts_events").unwrap();
        assert!(cols.contains("scheduled_at"), "date column should be added");
        assert!(
            cols.contains("scheduled_at_tz"),
            "timezone companion column should be added on alter"
        );
    }

    // ── inherited localization in join tables ────────────────────────────

    #[test]
    fn localized_group_array_inherits_locale_column() {
        // Regression: arrays inside localized Groups missed _locale column
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .localized(true)
                    .fields(vec![
                        FieldDefinition::builder("items", FieldType::Array)
                            .fields(vec![text_field("label")])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_meta__items").unwrap();
        assert!(
            cols.contains("_locale"),
            "Array inside localized Group should inherit _locale column"
        );
    }

    #[test]
    fn localized_group_blocks_inherits_locale_column() {
        // Regression: blocks inside localized Groups missed _locale column
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .localized(true)
                    .fields(vec![
                        FieldDefinition::builder("content", FieldType::Blocks).build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_meta__content").unwrap();
        assert!(
            cols.contains("_locale"),
            "Blocks inside localized Group should inherit _locale column"
        );
    }

    #[test]
    fn localized_group_has_many_inherits_locale_column() {
        // Regression: has-many relationships inside localized Groups missed _locale column
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .localized(true)
                    .fields(vec![
                        FieldDefinition::builder("tags", FieldType::Relationship)
                            .relationship(RelationshipConfig::new("tags", true))
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts_meta__tags").unwrap();
        assert!(
            cols.contains("_locale"),
            "has-many Relationship inside localized Group should inherit _locale column"
        );
    }

    // ── sync_versions_table ──────────────────────────────────────────────

    #[test]
    fn polymorphic_upgrade_rebuilds_primary_key() {
        let text = |s: &str| DbValue::Text(s.to_string());
        let int = |n: i64| DbValue::Integer(n);

        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        // Step 1: Create a non-polymorphic junction table (simulates old schema)
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute(
            "CREATE TABLE posts_related (\
            parent_id TEXT NOT NULL, \
            related_id TEXT NOT NULL, \
            _order INTEGER NOT NULL DEFAULT 0, \
            PRIMARY KEY (parent_id, related_id)\
        )",
            &[],
        )
        .unwrap();

        // Step 2: Insert parent row and junction data
        conn.execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();
        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, _order) VALUES (?1, ?2, ?3)",
            &[text("p1"), text("r1"), int(0)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO posts_related (parent_id, related_id, _order) VALUES (?1, ?2, ?3)",
            &[text("p1"), text("r2"), int(1)],
        )
        .unwrap();

        // Step 3: Run the upgrade (simulating schema change to polymorphic)
        let mut rc = RelationshipConfig::new("tags", true);
        rc.polymorphic = vec!["tags".into(), "categories".into()];

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("related", FieldType::Relationship)
                    .relationship(rc)
                    .build(),
            ],
        );
        sync_join_tables(&conn, "posts", &def.fields, &no_locale()).unwrap();

        // Step 4: Verify data is preserved
        let rows = conn
        .query_all("SELECT parent_id, related_id, related_collection, _order FROM posts_related ORDER BY _order", &[])
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get_string("parent_id").unwrap(), "p1");
        assert_eq!(rows[0].get_string("related_id").unwrap(), "r1");
        assert_eq!(rows[0].get_string("related_collection").unwrap(), "");
        assert_eq!(rows[1].get_string("related_id").unwrap(), "r2");

        // Step 5: Verify the new PK allows duplicate (parent_id, related_id)
        // with different related_collection values
        conn.execute(
        "INSERT INTO posts_related (parent_id, related_id, related_collection, _order) VALUES (?1, ?2, ?3, ?4)",
        &[text("p1"), text("r1"), text("categories"), int(2)],
    )
    .unwrap();

        let count = conn
            .query_all(
                "SELECT * FROM posts_related WHERE parent_id = ?1 AND related_id = ?2",
                &[text("p1"), text("r1")],
            )
            .unwrap();
        assert_eq!(
            count.len(),
            2,
            "Same (parent_id, related_id) with different related_collection should be allowed"
        );
    }

    #[test]
    fn polymorphic_upgrade_with_locale_preserves_data() {
        let text = |s: &str| DbValue::Text(s.to_string());
        let int = |n: i64| DbValue::Integer(n);

        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        // Create a non-polymorphic localized junction table
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY)", &[])
            .unwrap();
        conn.execute(
            "CREATE TABLE posts_related (\
            parent_id TEXT NOT NULL, \
            related_id TEXT NOT NULL, \
            _order INTEGER NOT NULL DEFAULT 0, \
            _locale TEXT NOT NULL DEFAULT 'en', \
            PRIMARY KEY (parent_id, related_id, _locale)\
        )",
            &[],
        )
        .unwrap();

        conn.execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();
        conn.execute(
        "INSERT INTO posts_related (parent_id, related_id, _order, _locale) VALUES (?1, ?2, ?3, ?4)",
        &[text("p1"), text("r1"), int(0), text("en")],
    )
    .unwrap();
        conn.execute(
        "INSERT INTO posts_related (parent_id, related_id, _order, _locale) VALUES (?1, ?2, ?3, ?4)",
        &[text("p1"), text("r1"), int(0), text("de")],
    )
    .unwrap();

        // Upgrade to polymorphic
        let mut rc = RelationshipConfig::new("tags", true);
        rc.polymorphic = vec!["tags".into(), "categories".into()];

        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("related", FieldType::Relationship)
                    .localized(true)
                    .relationship(rc)
                    .build(),
            ],
        );
        sync_join_tables(&conn, "posts", &def.fields, &locale_en_de()).unwrap();

        // Data preserved
        let rows = conn
            .query_all("SELECT * FROM posts_related ORDER BY _locale", &[])
            .unwrap();
        assert_eq!(rows.len(), 2);

        // related_collection column exists
        let cols = get_table_columns(&conn, "posts_related").unwrap();
        assert!(cols.contains("related_collection"));
        assert!(cols.contains("_locale"));
    }

    // ── polymorphic junction rebuild ──────────────────────────────────────

    #[test]
    fn polymorphic_junction_rebuild_preserves_fk() {
        // Regression: rebuild_junction_table_for_polymorphic dropped the
        // REFERENCES ... ON DELETE CASCADE constraint on parent_id.
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        // Create parent table
        conn.execute(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, _ref_count INTEGER DEFAULT 0)",
            &[],
        )
        .unwrap();

        // Create a non-polymorphic junction table with FK
        conn.execute_batch(
            "CREATE TABLE posts_tags (\
            parent_id TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE, \
            related_id TEXT NOT NULL, \
            _order INTEGER NOT NULL DEFAULT 0, \
            PRIMARY KEY (parent_id, related_id)\
        )",
        )
        .unwrap();

        // Insert test data
        conn.execute("INSERT INTO posts (id) VALUES ('p1')", &[])
            .unwrap();
        conn.execute(
            "INSERT INTO posts_tags (parent_id, related_id, _order) VALUES ('p1', 'tag1', 0)",
            &[],
        )
        .unwrap();

        // Rebuild for polymorphic upgrade
        rebuild_junction_table_for_polymorphic(&conn, "posts_tags", "posts", false).unwrap();

        // Verify columns
        let cols = get_table_columns(&conn, "posts_tags").unwrap();
        assert!(
            cols.contains("related_collection"),
            "must have related_collection"
        );

        // Verify data migrated
        let rows = conn
            .query_all("SELECT parent_id, related_id FROM posts_tags", &[])
            .unwrap();
        assert_eq!(rows.len(), 1);

        // Verify FK still works: cascade delete should remove junction row
        conn.execute("DELETE FROM posts WHERE id = 'p1'", &[])
            .unwrap();
        let rows = conn.query_all("SELECT * FROM posts_tags", &[]).unwrap();
        assert_eq!(
            rows.len(),
            0,
            "FK ON DELETE CASCADE must be preserved after polymorphic rebuild"
        );
    }
}
