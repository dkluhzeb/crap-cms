//! Global table sync: create and alter global tables from Lua definitions.

use anyhow::{Context as _, Result};

use crate::{config::LocaleConfig, core::collection::GlobalDefinition, db::DbConnection};

use crate::db::migrate::{
    collection::append_default_value,
    helpers::{
        collect_column_specs, get_table_columns, sanitize_locale, sync_join_tables,
        sync_versions_table, table_exists,
    },
};

pub(super) fn sync_global_table(
    conn: &dyn DbConnection,
    slug: &str,
    def: &GlobalDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let table_name = format!("_global_{}", slug);
    let exists = table_exists(conn, &table_name)?;

    if !exists {
        let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];

        for spec in &collect_column_specs(&def.fields, locale_config) {
            let col_type = if spec.companion_text {
                "TEXT"
            } else {
                conn.column_type_for(&spec.field.field_type)
            };

            if spec.is_localized {
                for locale in &locale_config.locales {
                    let mut col = format!(
                        "{}__{} {}",
                        spec.col_name,
                        sanitize_locale(locale)?,
                        col_type
                    );

                    if !spec.companion_text {
                        append_default_value(
                            &mut col,
                            &spec.field.default_value,
                            &spec.field.field_type,
                        );
                    }

                    columns.push(col);
                }
            } else {
                let mut col = format!("{} {}", spec.col_name, col_type);

                if !spec.companion_text {
                    append_default_value(
                        &mut col,
                        &spec.field.default_value,
                        &spec.field.field_type,
                    );
                }

                columns.push(col);
            }
        }

        // Versioned globals with drafts get a _status column
        if def.has_drafts() {
            columns.push("_status TEXT NOT NULL DEFAULT 'published'".to_string());
        }

        // All tables get a reference count for delete protection
        columns.push("_ref_count INTEGER NOT NULL DEFAULT 0".to_string());

        columns.push(format!("created_at {}", conn.timestamp_column_default()));
        columns.push(format!("updated_at {}", conn.timestamp_column_default()));

        let sql = format!("CREATE TABLE \"{}\" ({})", table_name, columns.join(", "));

        tracing::info!("Creating global table: {}", table_name);
        conn.execute(&sql, &[])
            .with_context(|| format!("Failed to create table {}", table_name))?;

        // Insert the single global row
        conn.execute(
            &conn.build_insert_ignore(&table_name, "id", "'default'"),
            &[],
        )?;
    } else {
        // ALTER TABLE: add columns for new scalar/group fields
        let existing_columns = get_table_columns(conn, &table_name)?;

        for spec in &collect_column_specs(&def.fields, locale_config) {
            let col_type = if spec.companion_text {
                "TEXT"
            } else {
                conn.column_type_for(&spec.field.field_type)
            };

            if spec.is_localized {
                for locale in &locale_config.locales {
                    let col_name = format!("{}__{}", spec.col_name, sanitize_locale(locale)?);

                    if !existing_columns.contains(&col_name) {
                        let mut col_def = col_type.to_string();

                        if !spec.companion_text {
                            append_default_value(
                                &mut col_def,
                                &spec.field.default_value,
                                &spec.field.field_type,
                            );
                        }

                        let sql = format!(
                            "ALTER TABLE \"{}\" ADD COLUMN {} {}",
                            table_name, col_name, col_def
                        );
                        tracing::info!("Adding column to {}: {}", table_name, col_name);
                        conn.execute(&sql, &[]).with_context(|| {
                            format!("Failed to add column {} to {}", col_name, table_name)
                        })?;
                    }
                }
            } else if !existing_columns.contains(&spec.col_name) {
                let mut col_def = col_type.to_string();

                if !spec.companion_text {
                    append_default_value(
                        &mut col_def,
                        &spec.field.default_value,
                        &spec.field.field_type,
                    );
                }

                let sql = format!(
                    "ALTER TABLE \"{}\" ADD COLUMN {} {}",
                    table_name, spec.col_name, col_def
                );
                tracing::info!("Adding column to {}: {}", table_name, spec.col_name);
                conn.execute(&sql, &[]).with_context(|| {
                    format!("Failed to add column {} to {}", spec.col_name, table_name)
                })?;
            }
        }
    }

    // Versioned globals with drafts: ensure _status column exists (ALTER path)
    if exists && def.has_drafts() {
        let existing_columns = get_table_columns(conn, &table_name)?;

        if !existing_columns.contains("_status") {
            let sql = format!(
                "ALTER TABLE \"{}\" ADD COLUMN _status TEXT NOT NULL DEFAULT 'published'",
                table_name
            );
            tracing::info!("Adding _status column to {}", table_name);
            conn.execute(&sql, &[])
                .with_context(|| format!("Failed to add _status to {}", table_name))?;
        }
    }

    // All globals: ensure _ref_count column exists for delete protection
    if exists {
        let existing_columns = get_table_columns(conn, &table_name)?;

        if !existing_columns.contains("_ref_count") {
            let sql = format!(
                "ALTER TABLE \"{}\" ADD COLUMN _ref_count INTEGER NOT NULL DEFAULT 0",
                table_name
            );
            tracing::info!("Adding _ref_count column to {}", table_name);
            conn.execute(&sql, &[])
                .with_context(|| format!("Failed to add _ref_count to {}", table_name))?;
        }
    }

    // Sync join tables for array, blocks, and has-many relationship fields
    sync_join_tables(conn, &table_name, &def.fields, locale_config)?;

    // Sync versions table if versioning is enabled
    if def.has_versions() {
        sync_versions_table(conn, &table_name)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CrapConfig, LocaleConfig};
    use crate::core::collection::*;
    use crate::core::field::{FieldDefinition, FieldType};
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

    fn simple_global(slug: &str, fields: Vec<FieldDefinition>) -> GlobalDefinition {
        let mut def = GlobalDefinition::new(slug);
        def.fields = fields;
        def
    }

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn localized_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .localized(true)
            .build()
    }

    // ── global table ──────────────────────────────────────────────────────

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
        let count = row.get_i64("cnt").unwrap();
        assert_eq!(count, 1, "should have exactly one default row");
    }

    // ── global table alter (add new field to existing global) ─────────────

    #[test]
    fn global_table_alter_adds_new_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("site_name")]);
        sync_global_table(&conn, "settings", &def1, &no_locale()).unwrap();

        // Now add a new field
        let def2 = simple_global(
            "settings",
            vec![text_field("site_name"), text_field("site_url")],
        );
        sync_global_table(&conn, "settings", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(
            cols.contains("site_url"),
            "New column should be added via ALTER"
        );
    }

    // ── global table with localized fields ──────────────────────────────

    #[test]
    fn global_table_localized_fields() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global("settings", vec![localized_field("site_name")]);
        sync_global_table(&conn, "settings", &def, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("site_name__en"));
        assert!(cols.contains("site_name__de"));
        assert!(!cols.contains("site_name"));
    }

    // ── global table alter with localized fields ────────────────────────

    #[test]
    fn global_table_alter_adds_localized_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &locale_en_de()).unwrap();

        // Add a localized field to existing table
        let def2 = simple_global(
            "settings",
            vec![text_field("name"), localized_field("description")],
        );
        sync_global_table(&conn, "settings", &def2, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("description__en"));
        assert!(cols.contains("description__de"));
    }

    // ── global table with group fields ──────────────────────────────────

    #[test]
    fn global_table_group_fields_create() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("seo", FieldType::Group)
                    .fields(vec![text_field("title"), text_field("description")])
                    .build(),
            ],
        );
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("seo__title"));
        assert!(cols.contains("seo__description"));
    }

    #[test]
    fn global_table_group_fields_alter() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &no_locale()).unwrap();

        // Add a group field to existing table
        let def2 = simple_global(
            "settings",
            vec![
                text_field("name"),
                FieldDefinition::builder("seo", FieldType::Group)
                    .fields(vec![text_field("title")])
                    .build(),
            ],
        );
        sync_global_table(&conn, "settings", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("seo__title"));
    }

    // ── global table with localized group fields ────────────────────────

    #[test]
    fn global_table_localized_group_create() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("seo", FieldType::Group)
                    .localized(true)
                    .fields(vec![text_field("title")])
                    .build(),
            ],
        );
        sync_global_table(&conn, "settings", &def, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    #[test]
    fn global_table_localized_group_alter() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &locale_en_de()).unwrap();

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
        sync_global_table(&conn, "settings", &def2, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    // ── versioned global table ──────────────────────────────────────────

    #[test]
    fn versioned_global_creates_versions_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_global("settings", vec![text_field("name")]);
        def.versions = Some(VersionsConfig::new(true, 5));
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        assert!(table_exists(&conn, "_versions__global_settings").unwrap());
        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(
            cols.contains("_status"),
            "Drafts global should have _status column"
        );
    }

    // ── global table alter adds _status for drafts ──────────────────────

    #[test]
    fn global_table_alter_adds_status_for_drafts() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &no_locale()).unwrap();

        let cols_before = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(!cols_before.contains("_status"));

        // Now enable drafts
        let mut def2 = simple_global("settings", vec![text_field("name")]);
        def2.versions = Some(VersionsConfig::new(true, 5));
        sync_global_table(&conn, "settings", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("_status"));
    }

    // ── global table with join tables ───────────────────────────────────

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

    // ── collapsible fields ──────────────────────────────────────────────

    #[test]
    fn global_table_collapsible_promotes_flat() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("extra", FieldType::Collapsible)
                    .fields(vec![text_field("notes"), text_field("footer")])
                    .build(),
            ],
        );
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("notes"));
        assert!(cols.contains("footer"));
        assert!(!cols.contains("extra"));
    }

    // ── tabs fields ─────────────────────────────────────────────────────

    #[test]
    fn global_table_tabs_promotes_flat() {
        use crate::core::field::FieldTab;
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
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
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("site_name"));
        assert!(cols.contains("copyright"));
        assert!(!cols.contains("layout"));
    }

    // ── tabs containing group (regression test) ─────────────────────────

    #[test]
    fn global_table_tabs_with_group_creates_prefixed_columns() {
        use crate::core::field::FieldTab;
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
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
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(
            cols.contains("social__github"),
            "Group inside Tabs should use group__subfield"
        );
        assert!(
            cols.contains("social__twitter"),
            "Group inside Tabs should use group__subfield"
        );
        assert!(
            !cols.contains("social"),
            "Group itself should not be a column"
        );
    }

    // ── collapsible containing group ────────────────────────────────────

    #[test]
    fn global_table_collapsible_with_group_creates_prefixed_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
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
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(
            cols.contains("seo__title"),
            "Group inside Collapsible should use group__subfield"
        );
        assert!(
            cols.contains("seo__desc"),
            "Group inside Collapsible should use group__subfield"
        );
    }

    // ── alter: add tabs with group to existing global ───────────────────

    #[test]
    fn global_table_alter_adds_tabs_with_group() {
        use crate::core::field::FieldTab;
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &no_locale()).unwrap();

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
        sync_global_table(&conn, "settings", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(
            cols.contains("social__github"),
            "ALTER should add group__subfield inside Tabs"
        );
    }

    // ── deeply nested: tabs → collapsible → group ───────────────────────

    #[test]
    fn global_deeply_nested_layout() {
        use crate::core::field::FieldTab;
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
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
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(
            cols.contains("og__image"),
            "Deeply nested: Tabs → Collapsible → Group"
        );
        assert!(
            cols.contains("canonical"),
            "Deeply nested: Tabs → Collapsible → plain"
        );
    }

    // ── Group containing layout fields (the former terminal-node bug) ─────

    #[test]
    fn global_group_containing_row() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
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
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("branding__logo"), "Group→Row: branding__logo");
        assert!(
            cols.contains("branding__favicon"),
            "Group→Row: branding__favicon"
        );
    }

    #[test]
    fn global_group_containing_tabs() {
        use crate::core::field::FieldTab;
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
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
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(
            cols.contains("config__site_name"),
            "Group→Tabs: config__site_name"
        );
        assert!(
            cols.contains("config__twitter"),
            "Group→Tabs: config__twitter"
        );
    }

    #[test]
    fn global_group_tabs_group_three_levels() {
        use crate::core::field::FieldTab;
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
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
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("a__b__leaf"), "Group→Tabs→Group: a__b__leaf");
    }

    // ── companion columns (timezone _tz) ────────────────────────────────

    #[test]
    fn global_table_date_timezone_creates_companion_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("event_at", FieldType::Date)
                    .timezone(true)
                    .build(),
            ],
        );
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("event_at"), "main date column");
        assert!(cols.contains("event_at_tz"), "companion _tz column");

        // Verify _tz column is TEXT, not the date column type
        let types = conn.get_table_column_types("_global_settings").unwrap();
        assert_eq!(
            types.get("event_at_tz").map(|s| s.as_str()),
            Some("TEXT"),
            "_tz companion column must be TEXT"
        );
    }

    #[test]
    fn global_table_alter_adds_date_timezone_companion_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &no_locale()).unwrap();

        let def2 = simple_global(
            "settings",
            vec![
                text_field("name"),
                FieldDefinition::builder("event_at", FieldType::Date)
                    .timezone(true)
                    .build(),
            ],
        );
        sync_global_table(&conn, "settings", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(
            cols.contains("event_at_tz"),
            "ALTER should add _tz companion"
        );

        let types = conn.get_table_column_types("_global_settings").unwrap();
        assert_eq!(
            types.get("event_at_tz").map(|s| s.as_str()),
            Some("TEXT"),
            "ALTER _tz companion column must be TEXT"
        );
    }

    #[test]
    fn global_table_localized_date_timezone_creates_companion_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global(
            "settings",
            vec![
                FieldDefinition::builder("event_at", FieldType::Date)
                    .timezone(true)
                    .localized(true)
                    .build(),
            ],
        );
        sync_global_table(&conn, "settings", &def, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("event_at__en"));
        assert!(cols.contains("event_at__de"));
        assert!(cols.contains("event_at_tz__en"));
        assert!(cols.contains("event_at_tz__de"));

        let types = conn.get_table_column_types("_global_settings").unwrap();
        assert_eq!(
            types.get("event_at_tz__en").map(|s| s.as_str()),
            Some("TEXT")
        );
        assert_eq!(
            types.get("event_at_tz__de").map(|s| s.as_str()),
            Some("TEXT")
        );
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
