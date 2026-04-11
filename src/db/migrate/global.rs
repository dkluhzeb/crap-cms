//! Global table sync: create and alter global tables from Lua definitions.

use anyhow::{Context as _, Result};
use std::collections::HashSet;
use tracing::info;

use crate::{
    config::LocaleConfig,
    core::collection::GlobalDefinition,
    db::{
        DbConnection,
        query::helpers::{global_table, locale_column},
    },
};

use crate::db::migrate::{
    collection::append_default_value_for,
    helpers::{
        collect_column_specs, get_table_columns, sync_join_tables, sync_versions_table,
        table_exists,
    },
};

/// Sync a global's schema: create or alter table, join tables, versions.
pub(super) fn sync_global_table(
    conn: &dyn DbConnection,
    slug: &str,
    def: &GlobalDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let table_name = global_table(slug);

    if table_exists(conn, &table_name)? {
        alter_global_table(conn, &table_name, def, locale_config)?;
    } else {
        create_global_table(conn, &table_name, def, locale_config)?;
    }

    sync_join_tables(conn, &table_name, &def.fields, locale_config)?;

    if def.has_versions() {
        sync_versions_table(conn, &table_name)?;
    }

    Ok(())
}

/// Build a column definition with optional default value.
fn build_col_def(
    col_name: &str,
    col_type: &str,
    companion_text: bool,
    field: &crate::core::FieldDefinition,
    db_kind: &str,
) -> String {
    let mut col = format!("{} {}", col_name, col_type);

    if !companion_text {
        append_default_value_for(&mut col, &field.default_value, &field.field_type, db_kind);
    }

    col
}

/// Create a new global table with all field columns and a default row.
fn create_global_table(
    conn: &dyn DbConnection,
    table_name: &str,
    def: &GlobalDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];

    for spec in &collect_column_specs(&def.fields, locale_config) {
        let col_type = if spec.companion_text {
            "TEXT"
        } else {
            conn.column_type_for(&spec.field.field_type)
        };

        if spec.is_localized {
            for locale in &locale_config.locales {
                let col_name = locale_column(&spec.col_name, locale)?;
                columns.push(build_col_def(
                    &col_name,
                    col_type,
                    spec.companion_text,
                    spec.field,
                    conn.kind(),
                ));
            }
        } else {
            columns.push(build_col_def(
                &spec.col_name,
                col_type,
                spec.companion_text,
                spec.field,
                conn.kind(),
            ));
        }
    }

    if def.has_drafts() {
        columns.push("_status TEXT NOT NULL DEFAULT 'published'".to_string());
    }

    columns.push("_ref_count INTEGER NOT NULL DEFAULT 0".to_string());
    columns.push(format!("created_at {}", conn.timestamp_column_default()));
    columns.push(format!("updated_at {}", conn.timestamp_column_default()));

    let sql = format!("CREATE TABLE \"{}\" ({})", table_name, columns.join(", "));

    info!("Creating global table: {}", table_name);

    conn.execute_ddl(&sql, &[])
        .with_context(|| format!("Failed to create table {}", table_name))?;

    conn.execute(
        &conn.build_insert_ignore(table_name, "id", "'default'"),
        &[],
    )?;

    Ok(())
}

/// Add missing columns to an existing global table.
fn alter_global_table(
    conn: &dyn DbConnection,
    table_name: &str,
    def: &GlobalDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let existing = get_table_columns(conn, table_name)?;

    add_field_columns(conn, table_name, def, locale_config, &existing)?;
    add_system_column(
        conn,
        table_name,
        "_status",
        "TEXT NOT NULL DEFAULT 'published'",
        def.has_drafts(),
        &existing,
    )?;
    add_system_column(
        conn,
        table_name,
        "_ref_count",
        "INTEGER NOT NULL DEFAULT 0",
        true,
        &existing,
    )?;

    Ok(())
}

/// Add missing field columns to a global table.
fn add_field_columns(
    conn: &dyn DbConnection,
    table_name: &str,
    def: &GlobalDefinition,
    locale_config: &LocaleConfig,
    existing: &HashSet<String>,
) -> Result<()> {
    for spec in &collect_column_specs(&def.fields, locale_config) {
        let col_type = if spec.companion_text {
            "TEXT"
        } else {
            conn.column_type_for(&spec.field.field_type)
        };

        if spec.is_localized {
            for locale in &locale_config.locales {
                let col_name = locale_column(&spec.col_name, locale)?;
                add_column_if_missing(
                    conn,
                    table_name,
                    &col_name,
                    col_type,
                    spec.companion_text,
                    spec.field,
                    existing,
                )?;
            }
        } else {
            add_column_if_missing(
                conn,
                table_name,
                &spec.col_name,
                col_type,
                spec.companion_text,
                spec.field,
                existing,
            )?;
        }
    }

    Ok(())
}

/// Add a single column if it doesn't already exist.
fn add_column_if_missing(
    conn: &dyn DbConnection,
    table_name: &str,
    col_name: &str,
    col_type: &str,
    companion_text: bool,
    field: &crate::core::FieldDefinition,
    existing: &HashSet<String>,
) -> Result<()> {
    if existing.contains(col_name) {
        return Ok(());
    }

    let col_def = build_col_def(col_name, col_type, companion_text, field, conn.kind());
    let sql = format!("ALTER TABLE \"{}\" ADD COLUMN {}", table_name, col_def);

    info!("Adding column to {}: {}", table_name, col_name);

    conn.execute_ddl(&sql, &[])
        .with_context(|| format!("Failed to add column {} to {}", col_name, table_name))?;

    Ok(())
}

/// Add a system column if condition is true and column doesn't exist.
fn add_system_column(
    conn: &dyn DbConnection,
    table_name: &str,
    col_name: &str,
    col_def: &str,
    condition: bool,
    existing: &HashSet<String>,
) -> Result<()> {
    if !condition || existing.contains(col_name) {
        return Ok(());
    }

    let sql = format!(
        "ALTER TABLE \"{}\" ADD COLUMN {} {}",
        table_name, col_name, col_def
    );

    info!("Adding {} column to {}", col_name, table_name);

    conn.execute_ddl(&sql, &[])
        .with_context(|| format!("Failed to add {} to {}", col_name, table_name))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;
    use crate::core::field::{FieldDefinition, FieldType};
    use crate::db::migrate::collection::test_helpers::*;

    fn simple_global(slug: &str, fields: Vec<FieldDefinition>) -> GlobalDefinition {
        let mut def = GlobalDefinition::new(slug);
        def.fields = fields;
        def
    }

    /// Sync a global and return its column names.
    fn sync_and_columns(def: &GlobalDefinition, locale: &LocaleConfig) -> HashSet<String> {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        sync_global_table(&conn, &def.slug, def, locale).unwrap();
        get_table_columns(&conn, &global_table(&def.slug)).unwrap()
    }

    /// Sync two defs sequentially (create then alter) and return columns.
    fn sync_alter_and_columns(
        def1: &GlobalDefinition,
        def2: &GlobalDefinition,
        locale: &LocaleConfig,
    ) -> HashSet<String> {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        sync_global_table(&conn, &def1.slug, def1, locale).unwrap();
        sync_global_table(&conn, &def1.slug, def2, locale).unwrap();
        get_table_columns(&conn, &global_table(&def1.slug)).unwrap()
    }

    // ── create ──────────────────────────────────────────────────────────

    #[test]
    fn global_table_created_with_default_row() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global("settings", vec![text_field("site_name")]);
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        assert!(table_exists(&conn, "_global_settings").unwrap());
        let row = conn
            .query_one("SELECT COUNT(*) AS cnt FROM _global_settings", &[])
            .unwrap()
            .unwrap();
        assert_eq!(row.get_i64("cnt").unwrap(), 1);
    }

    // ── alter ───────────────────────────────────────────────────────────

    #[test]
    fn global_table_alter_adds_new_column() {
        let def1 = simple_global("settings", vec![text_field("site_name")]);
        let def2 = simple_global(
            "settings",
            vec![text_field("site_name"), text_field("site_url")],
        );
        let cols = sync_alter_and_columns(&def1, &def2, &no_locale());
        assert!(cols.contains("site_url"));
    }

    // ── localized ───────────────────────────────────────────────────────

    #[test]
    fn global_table_localized_fields() {
        let def = simple_global("settings", vec![localized_field("site_name")]);
        let cols = sync_and_columns(&def, &locale_en_de());
        assert!(cols.contains("site_name__en"));
        assert!(cols.contains("site_name__de"));
        assert!(!cols.contains("site_name"));
    }

    #[test]
    fn global_table_alter_adds_localized_columns() {
        let def1 = simple_global("settings", vec![text_field("name")]);
        let def2 = simple_global(
            "settings",
            vec![text_field("name"), localized_field("description")],
        );
        let cols = sync_alter_and_columns(&def1, &def2, &locale_en_de());
        assert!(cols.contains("description__en"));
        assert!(cols.contains("description__de"));
    }

    // ── group fields ────────────────────────────────────────────────────

    #[test]
    fn global_table_group_fields_create() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("seo", FieldType::Group)
                    .fields(vec![text_field("title"), text_field("description")])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("seo__title"));
        assert!(cols.contains("seo__description"));
    }

    #[test]
    fn global_table_group_fields_alter() {
        let def1 = simple_global("settings", vec![text_field("name")]);
        let def2 = simple_global(
            "settings",
            vec![
                text_field("name"),
                FieldDefinition::builder("seo", FieldType::Group)
                    .fields(vec![text_field("title")])
                    .build(),
            ],
        );
        let cols = sync_alter_and_columns(&def1, &def2, &no_locale());
        assert!(cols.contains("seo__title"));
    }

    // ── localized group fields ──────────────────────────────────────────

    #[test]
    fn global_table_localized_group_create() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("seo", FieldType::Group)
                    .localized(true)
                    .fields(vec![text_field("title")])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &locale_en_de());
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    #[test]
    fn global_table_localized_group_alter() {
        let def1 = simple_global("settings", vec![text_field("name")]);
        let def2 = simple_global(
            "settings",
            vec![
                text_field("name"),
                FieldDefinition::builder("seo", FieldType::Group)
                    .localized(true)
                    .fields(vec![text_field("title")])
                    .build(),
            ],
        );
        let cols = sync_alter_and_columns(&def1, &def2, &locale_en_de());
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    // ── versioned ───────────────────────────────────────────────────────

    #[test]
    fn versioned_global_creates_versions_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_global("settings", vec![text_field("name")]);
        def.versions = Some(VersionsConfig::new(true, 5));
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();
        assert!(table_exists(&conn, "_versions__global_settings").unwrap());
        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("_status"));
    }

    #[test]
    fn global_table_alter_adds_status_for_drafts() {
        let def1 = simple_global("settings", vec![text_field("name")]);
        let mut def2 = simple_global("settings", vec![text_field("name")]);
        def2.versions = Some(VersionsConfig::new(true, 5));
        let cols = sync_alter_and_columns(&def1, &def2, &no_locale());
        assert!(cols.contains("_status"));
    }

    // ── join tables ─────────────────────────────────────────────────────

    #[test]
    fn global_table_creates_join_tables() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("items", FieldType::Array)
                    .fields(vec![text_field("label")])
                    .build(),
            ],
        );
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();
        assert!(table_exists(&conn, "_global_settings_items").unwrap());
    }

    // ── layout wrappers ─────────────────────────────────────────────────

    #[test]
    fn global_table_collapsible_promotes_flat() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("extra", FieldType::Collapsible)
                    .fields(vec![text_field("notes"), text_field("footer")])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("notes"));
        assert!(cols.contains("footer"));
        assert!(!cols.contains("extra"));
    }

    #[test]
    fn global_table_tabs_promotes_flat() {
        use crate::core::field::FieldTab;
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![
                        FieldTab::new("General", vec![text_field("site_name")]),
                        FieldTab::new("Footer", vec![text_field("copyright")]),
                    ])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("site_name"));
        assert!(cols.contains("copyright"));
        assert!(!cols.contains("layout"));
    }

    #[test]
    fn global_table_tabs_with_group_creates_prefixed_columns() {
        use crate::core::field::FieldTab;
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "Social",
                        vec![
                            FieldDefinition::builder("social", FieldType::Group)
                                .fields(vec![text_field("github"), text_field("twitter")])
                                .build(),
                        ],
                    )])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("social__github"));
        assert!(cols.contains("social__twitter"));
        assert!(!cols.contains("social"));
    }

    #[test]
    fn global_table_collapsible_with_group_creates_prefixed_columns() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("extra", FieldType::Collapsible)
                    .fields(vec![
                        FieldDefinition::builder("seo", FieldType::Group)
                            .fields(vec![text_field("title"), text_field("desc")])
                            .build(),
                    ])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("seo__title"));
        assert!(cols.contains("seo__desc"));
    }

    #[test]
    fn global_table_alter_adds_tabs_with_group() {
        use crate::core::field::FieldTab;
        let def1 = simple_global("settings", vec![text_field("name")]);
        let def2 = simple_global(
            "settings",
            vec![
                text_field("name"),
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "Social",
                        vec![
                            FieldDefinition::builder("social", FieldType::Group)
                                .fields(vec![text_field("github")])
                                .build(),
                        ],
                    )])
                    .build(),
            ],
        );
        let cols = sync_alter_and_columns(&def1, &def2, &no_locale());
        assert!(cols.contains("social__github"));
    }

    // ── deeply nested ───────────────────────────────────────────────────

    #[test]
    fn global_deeply_nested_layout() {
        use crate::core::field::FieldTab;
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "Advanced",
                        vec![
                            FieldDefinition::builder("advanced", FieldType::Collapsible)
                                .fields(vec![
                                    FieldDefinition::builder("og", FieldType::Group)
                                        .fields(vec![text_field("image")])
                                        .build(),
                                    text_field("canonical"),
                                ])
                                .build(),
                        ],
                    )])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("og__image"));
        assert!(cols.contains("canonical"));
    }

    #[test]
    fn global_group_containing_row() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("branding", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("row1", FieldType::Row)
                            .fields(vec![text_field("logo"), text_field("favicon")])
                            .build(),
                    ])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("branding__logo"));
        assert!(cols.contains("branding__favicon"));
    }

    #[test]
    fn global_group_containing_tabs() {
        use crate::core::field::FieldTab;
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("config", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("layout", FieldType::Tabs)
                            .tabs(vec![
                                FieldTab::new("General", vec![text_field("site_name")]),
                                FieldTab::new("Social", vec![text_field("twitter")]),
                            ])
                            .build(),
                    ])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("config__site_name"));
        assert!(cols.contains("config__twitter"));
    }

    #[test]
    fn global_group_tabs_group_three_levels() {
        use crate::core::field::FieldTab;
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("a", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("t", FieldType::Tabs)
                            .tabs(vec![FieldTab::new(
                                "Tab",
                                vec![
                                    FieldDefinition::builder("b", FieldType::Group)
                                        .fields(vec![text_field("leaf")])
                                        .build(),
                                ],
                            )])
                            .build(),
                    ])
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("a__b__leaf"));
    }

    // ── companion columns (timezone _tz) ────────────────────────────────

    #[test]
    fn global_table_date_timezone_creates_companion_column() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("event_at", FieldType::Date)
                    .timezone(true)
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &no_locale());
        assert!(cols.contains("event_at"));
        assert!(cols.contains("event_at_tz"));
    }

    #[test]
    fn global_table_alter_adds_date_timezone_companion_column() {
        let def1 = simple_global("settings", vec![text_field("name")]);
        let def2 = simple_global(
            "settings",
            vec![
                text_field("name"),
                FieldDefinition::builder("event_at", FieldType::Date)
                    .timezone(true)
                    .build(),
            ],
        );
        let cols = sync_alter_and_columns(&def1, &def2, &no_locale());
        assert!(cols.contains("event_at_tz"));
    }

    #[test]
    fn global_table_localized_date_timezone_creates_companion_columns() {
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("event_at", FieldType::Date)
                    .timezone(true)
                    .localized(true)
                    .build(),
            ],
        );
        let cols = sync_and_columns(&def, &locale_en_de());
        assert!(cols.contains("event_at__en"));
        assert!(cols.contains("event_at__de"));
        assert!(cols.contains("event_at_tz__en"));
        assert!(cols.contains("event_at_tz__de"));
    }

    // ── default values ──────────────────────────────────────────────────

    #[test]
    fn global_table_creates_with_default_values() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("site_name", FieldType::Text)
                    .default_value(serde_json::Value::String("My Site".to_string()))
                    .build(),
                FieldDefinition::builder("enabled", FieldType::Checkbox).build(),
            ],
        );
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        // SQLite inserts NULL for the default row (INSERT OR IGNORE with just id),
        // but the column DEFAULT is correctly set. Verify by inserting a new row.
        conn.execute_batch("INSERT INTO _global_settings (id) VALUES ('test_defaults')")
            .unwrap();
        let row = conn
            .query_one(
                "SELECT site_name, enabled FROM _global_settings WHERE id = 'test_defaults'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            row.get_opt_string("site_name").unwrap(),
            Some("My Site".to_string()),
            "Text field should have DEFAULT applied"
        );
        assert_eq!(
            row.get_i64("enabled").unwrap(),
            0,
            "Checkbox should have DEFAULT 0"
        );
    }

    #[test]
    fn global_table_alter_adds_column_with_default() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &no_locale()).unwrap();

        let def2 = simple_global(
            "settings",
            vec![
                text_field("name"),
                FieldDefinition::builder("mode", FieldType::Text)
                    .default_value(serde_json::Value::String("dark".to_string()))
                    .build(),
            ],
        );
        sync_global_table(&conn, "settings", &def2, &no_locale()).unwrap();

        // Verify default by inserting a row that relies on DEFAULT
        conn.execute_batch("INSERT INTO _global_settings (id) VALUES ('test_alter_default')")
            .unwrap();
        let row = conn
            .query_one(
                "SELECT mode FROM _global_settings WHERE id = 'test_alter_default'",
                &[],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            row.get_opt_string("mode").unwrap(),
            Some("dark".to_string()),
            "ALTER-added column should have DEFAULT applied"
        );
    }
}
