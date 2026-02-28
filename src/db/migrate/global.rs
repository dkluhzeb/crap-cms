//! Global table sync: create and alter global tables from Lua definitions.

use anyhow::{Context, Result};

use crate::config::LocaleConfig;
use crate::core::field::FieldType;

use super::helpers::{table_exists, get_table_columns, sync_join_tables, sync_versions_table};

pub(super) fn sync_global_table(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &crate::core::collection::GlobalDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let table_name = format!("_global_{}", slug);
    let exists = table_exists(conn, &table_name)?;

    if !exists {
        let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];

        for field in &def.fields {
            // Group fields expand sub-fields as prefixed columns (same as collections)
            if field.field_type == FieldType::Group {
                for sub in &field.fields {
                    let base_col_name = format!("{}__{}", field.name, sub.name);
                    let is_localized = (field.localized || sub.localized) && locale_config.is_enabled();

                    if is_localized {
                        for locale in &locale_config.locales {
                            let col = format!("{}__{} {}", base_col_name, locale, sub.field_type.sqlite_type());
                            columns.push(col);
                        }
                    } else {
                        let col = format!("{} {}", base_col_name, sub.field_type.sqlite_type());
                        columns.push(col);
                    }
                }
                continue;
            }
            // Skip fields that use join tables (array, blocks, has-many relationship).
            if !field.has_parent_column() {
                continue;
            }

            if field.localized && locale_config.is_enabled() {
                for locale in &locale_config.locales {
                    let col = format!("{}__{} {}", field.name, locale, field.field_type.sqlite_type());
                    columns.push(col);
                }
            } else {
                let col = format!("{} {}", field.name, field.field_type.sqlite_type());
                columns.push(col);
            }
        }

        // Versioned globals with drafts get a _status column
        if def.has_drafts() {
            columns.push("_status TEXT NOT NULL DEFAULT 'published'".to_string());
        }

        columns.push("created_at TEXT DEFAULT (datetime('now'))".to_string());
        columns.push("updated_at TEXT DEFAULT (datetime('now'))".to_string());

        let sql = format!(
            "CREATE TABLE {} ({})",
            table_name,
            columns.join(", ")
        );

        tracing::info!("Creating global table: {}", table_name);
        conn.execute(&sql, [])
            .with_context(|| format!("Failed to create table {}", table_name))?;

        // Insert the single global row
        conn.execute(
            &format!("INSERT OR IGNORE INTO {} (id) VALUES ('default')", table_name),
            [],
        )?;
    } else {
        // ALTER TABLE: add columns for new scalar/group fields
        let existing_columns = get_table_columns(conn, &table_name)?;

        for field in &def.fields {
            // Group fields expand sub-fields as prefixed columns (same as collections)
            if field.field_type == FieldType::Group {
                for sub in &field.fields {
                    let base_col_name = format!("{}__{}", field.name, sub.name);
                    let is_localized = (field.localized || sub.localized) && locale_config.is_enabled();

                    if is_localized {
                        for locale in &locale_config.locales {
                            let col_name = format!("{}__{}", base_col_name, locale);
                            if !existing_columns.contains(&col_name) {
                                let sql = format!(
                                    "ALTER TABLE {} ADD COLUMN {} {}",
                                    table_name, col_name, sub.field_type.sqlite_type()
                                );
                                tracing::info!("Adding column to {}: {}", table_name, col_name);
                                conn.execute(&sql, [])
                                    .with_context(|| format!("Failed to add column {} to {}", col_name, table_name))?;
                            }
                        }
                    } else if !existing_columns.contains(&base_col_name) {
                        let sql = format!(
                            "ALTER TABLE {} ADD COLUMN {} {}",
                            table_name, base_col_name, sub.field_type.sqlite_type()
                        );
                        tracing::info!("Adding column to {}: {}", table_name, base_col_name);
                        conn.execute(&sql, [])
                            .with_context(|| format!("Failed to add column {} to {}", base_col_name, table_name))?;
                    }
                }
                continue;
            }
            // Skip fields that use join tables (array, blocks, has-many relationship).
            if !field.has_parent_column() {
                continue;
            }

            if field.localized && locale_config.is_enabled() {
                for locale in &locale_config.locales {
                    let col_name = format!("{}__{}", field.name, locale);
                    if !existing_columns.contains(&col_name) {
                        let sql = format!(
                            "ALTER TABLE {} ADD COLUMN {} {}",
                            table_name, col_name, field.field_type.sqlite_type()
                        );
                        tracing::info!("Adding column to {}: {}", table_name, col_name);
                        conn.execute(&sql, [])
                            .with_context(|| format!("Failed to add column {} to {}", col_name, table_name))?;
                    }
                }
            } else if !existing_columns.contains(&field.name) {
                let sql = format!(
                    "ALTER TABLE {} ADD COLUMN {} {}",
                    table_name, field.name, field.field_type.sqlite_type()
                );
                tracing::info!("Adding column to {}: {}", table_name, field.name);
                conn.execute(&sql, [])
                    .with_context(|| format!("Failed to add column {} to {}", field.name, table_name))?;
            }
        }
    }

    // Versioned globals with drafts: ensure _status column exists (ALTER path)
    if exists && def.has_drafts() {
        let existing_columns = get_table_columns(conn, &table_name)?;
        if !existing_columns.contains("_status") {
            let sql = format!("ALTER TABLE {} ADD COLUMN _status TEXT NOT NULL DEFAULT 'published'", table_name);
            tracing::info!("Adding _status column to {}", table_name);
            conn.execute(&sql, [])
                .with_context(|| format!("Failed to add _status to {}", table_name))?;
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
    use crate::config::LocaleConfig;
    use crate::core::collection::*;
    use crate::core::field::{FieldDefinition, FieldType};
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

    fn simple_global(slug: &str, fields: Vec<FieldDefinition>) -> GlobalDefinition {
        GlobalDefinition {
            slug: slug.to_string(),
            labels: CollectionLabels::default(),
            fields,
            hooks: CollectionHooks::default(),
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

    fn localized_field(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Text,
            localized: true,
            ..Default::default()
        }
    }

    // ── global table ──────────────────────────────────────────────────────

    #[test]
    fn global_table_created_with_default_row() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global("settings", vec![text_field("site_name")]);
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        assert!(table_exists(&conn, "_global_settings").unwrap());
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM _global_settings", [], |r| r.get(0)
        ).unwrap();
        assert_eq!(count, 1, "should have exactly one default row");
    }

    // ── global table alter (add new field to existing global) ─────────────

    #[test]
    fn global_table_alter_adds_new_column() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("site_name")]);
        sync_global_table(&conn, "settings", &def1, &no_locale()).unwrap();

        // Now add a new field
        let def2 = simple_global("settings", vec![
            text_field("site_name"),
            text_field("site_url"),
        ]);
        sync_global_table(&conn, "settings", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("site_url"), "New column should be added via ALTER");
    }

    // ── global table with localized fields ──────────────────────────────

    #[test]
    fn global_table_localized_fields() {
        let pool = in_memory_pool();
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
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &locale_en_de()).unwrap();

        // Add a localized field to existing table
        let def2 = simple_global("settings", vec![
            text_field("name"),
            localized_field("description"),
        ]);
        sync_global_table(&conn, "settings", &def2, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("description__en"));
        assert!(cols.contains("description__de"));
    }

    // ── global table with group fields ──────────────────────────────────

    #[test]
    fn global_table_group_fields_create() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global("settings", vec![
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![text_field("title"), text_field("description")],
                ..Default::default()
            },
        ]);
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("seo__title"));
        assert!(cols.contains("seo__description"));
    }

    #[test]
    fn global_table_group_fields_alter() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &no_locale()).unwrap();

        // Add a group field to existing table
        let def2 = simple_global("settings", vec![
            text_field("name"),
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![text_field("title")],
                ..Default::default()
            },
        ]);
        sync_global_table(&conn, "settings", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("seo__title"));
    }

    // ── global table with localized group fields ────────────────────────

    #[test]
    fn global_table_localized_group_create() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global("settings", vec![
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                localized: true,
                fields: vec![text_field("title")],
                ..Default::default()
            },
        ]);
        sync_global_table(&conn, "settings", &def, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    #[test]
    fn global_table_localized_group_alter() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &locale_en_de()).unwrap();

        let def2 = simple_global("settings", vec![
            text_field("name"),
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                localized: true,
                fields: vec![text_field("title")],
                ..Default::default()
            },
        ]);
        sync_global_table(&conn, "settings", &def2, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    // ── versioned global table ──────────────────────────────────────────

    #[test]
    fn versioned_global_creates_versions_table() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_global("settings", vec![text_field("name")]);
        def.versions = Some(VersionsConfig { drafts: true, max_versions: 5 });
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        assert!(table_exists(&conn, "_versions__global_settings").unwrap());
        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("_status"), "Drafts global should have _status column");
    }

    // ── global table alter adds _status for drafts ──────────────────────

    #[test]
    fn global_table_alter_adds_status_for_drafts() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_global("settings", vec![text_field("name")]);
        sync_global_table(&conn, "settings", &def1, &no_locale()).unwrap();

        let cols_before = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(!cols_before.contains("_status"));

        // Now enable drafts
        let mut def2 = simple_global("settings", vec![text_field("name")]);
        def2.versions = Some(VersionsConfig { drafts: true, max_versions: 5 });
        sync_global_table(&conn, "settings", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "_global_settings").unwrap();
        assert!(cols.contains("_status"));
    }

    // ── global table with join tables ───────────────────────────────────

    #[test]
    fn global_table_creates_join_tables() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_global("settings", vec![
            FieldDefinition {
                name: "items".to_string(),
                field_type: FieldType::Array,
                fields: vec![text_field("label")],
                ..Default::default()
            },
        ]);
        sync_global_table(&conn, "settings", &def, &no_locale()).unwrap();

        assert!(table_exists(&conn, "_global_settings_items").unwrap());
    }
}
