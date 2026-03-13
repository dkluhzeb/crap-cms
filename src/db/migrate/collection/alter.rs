//! ALTER TABLE operations for existing collection tables.

use anyhow::{Context as _, Result};
use std::collections::HashSet;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, field::FieldType},
    db::migrate::helpers::{collect_column_specs, get_table_columns, sanitize_locale},
};

pub(super) fn alter_collection_table(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    // Get existing columns
    let existing_columns = get_table_columns(conn, slug)?;

    for spec in &collect_column_specs(&def.fields, locale_config) {
        if spec.is_localized {
            for locale in &locale_config.locales {
                let col_name = format!("{}__{}", spec.col_name, sanitize_locale(locale));

                if !existing_columns.contains(&col_name) {
                    let mut col_def = spec.field.field_type.sqlite_type().to_string();
                    super::create::append_default_value(
                        &mut col_def,
                        &spec.field.default_value,
                        &spec.field.field_type,
                    );
                    let sql = format!("ALTER TABLE {} ADD COLUMN {} {}", slug, col_name, col_def);
                    tracing::info!("Adding column to {}: {}", slug, col_name);
                    conn.execute(&sql, []).with_context(|| {
                        format!("Failed to add column {} to {}", col_name, slug)
                    })?;
                }
            }
        } else if !existing_columns.contains(&spec.col_name) {
            let mut col_def = spec.field.field_type.sqlite_type().to_string();
            super::create::append_default_value(
                &mut col_def,
                &spec.field.default_value,
                &spec.field.field_type,
            );
            let sql = format!(
                "ALTER TABLE {} ADD COLUMN {} {}",
                slug, spec.col_name, col_def
            );
            tracing::info!("Adding column to {}: {}", slug, spec.col_name);
            conn.execute(&sql, [])
                .with_context(|| format!("Failed to add column {} to {}", spec.col_name, slug))?;
        }
    }

    // Versioned collections with drafts: ensure _status column exists
    if def.has_drafts() && !existing_columns.contains("_status") {
        let sql = format!(
            "ALTER TABLE {} ADD COLUMN _status TEXT NOT NULL DEFAULT 'published'",
            slug
        );
        tracing::info!("Adding _status column to {}", slug);
        conn.execute(&sql, [])
            .with_context(|| format!("Failed to add _status to {}", slug))?;
    }

    // Auth collections: ensure system columns exist
    if def.is_auth_collection() {
        for col in [
            "_password_hash TEXT",
            "_reset_token TEXT",
            "_reset_token_exp INTEGER",
            "_locked INTEGER DEFAULT 0",
            "_settings TEXT",
        ] {
            let col_name = col
                .split_whitespace()
                .next()
                .expect("static column definition");

            if !existing_columns.contains(col_name) {
                let sql = format!("ALTER TABLE {} ADD COLUMN {}", slug, col);
                tracing::info!("Adding {} column to {}", col_name, slug);
                conn.execute(&sql, [])
                    .with_context(|| format!("Failed to add {} to {}", col_name, slug))?;
            }
        }
        if def.auth.as_ref().is_some_and(|a| a.verify_email) {
            for col in [
                "_verified INTEGER DEFAULT 0",
                "_verification_token TEXT",
                "_verification_token_exp INTEGER",
            ] {
                let col_name = col
                    .split_whitespace()
                    .next()
                    .expect("static column definition");

                if !existing_columns.contains(col_name) {
                    let sql = format!("ALTER TABLE {} ADD COLUMN {}", slug, col);
                    tracing::info!("Adding {} column to {}", col_name, slug);
                    conn.execute(&sql, [])
                        .with_context(|| format!("Failed to add {} to {}", col_name, slug))?;
                }
            }
        }
    }

    // Timestamps: ensure created_at/updated_at exist when timestamps are enabled
    // Note: SQLite ALTER TABLE cannot use non-constant defaults like datetime('now'),
    // so we add with no default (NULL for existing rows) — new inserts set these explicitly.
    if def.timestamps {
        for col_name in ["created_at", "updated_at"] {
            if !existing_columns.contains(col_name) {
                let sql = format!("ALTER TABLE {} ADD COLUMN {} TEXT", slug, col_name);
                tracing::info!("Adding {} column to {}", col_name, slug);
                conn.execute(&sql, [])
                    .with_context(|| format!("Failed to add {} to {}", col_name, slug))?;
            }
        }
    }

    // Warn about removed columns (SQLite can't DROP COLUMN easily)
    let mut field_names: HashSet<String> = HashSet::new();
    for f in &def.fields {
        if f.field_type == FieldType::Group {
            for sub in &f.fields {
                let base = format!("{}__{}", f.name, sub.name);
                let is_localized = (f.localized || sub.localized) && locale_config.is_enabled();

                if is_localized {
                    for locale in &locale_config.locales {
                        field_names.insert(format!("{}__{}", base, locale));
                    }
                } else {
                    field_names.insert(base);
                }
            }
        } else if f.field_type == FieldType::Row || f.field_type == FieldType::Collapsible {
            for sub in &f.fields {
                let is_localized = sub.localized && locale_config.is_enabled();

                if is_localized {
                    for locale in &locale_config.locales {
                        field_names.insert(format!("{}__{}", sub.name, locale));
                    }
                } else {
                    field_names.insert(sub.name.clone());
                }
            }
        } else if f.field_type == FieldType::Tabs {
            for tab in &f.tabs {
                for sub in &tab.fields {
                    let is_localized = sub.localized && locale_config.is_enabled();

                    if is_localized {
                        for locale in &locale_config.locales {
                            field_names.insert(format!("{}__{}", sub.name, locale));
                        }
                    } else {
                        field_names.insert(sub.name.clone());
                    }
                }
            }
        } else if f.has_parent_column() {
            if f.localized && locale_config.is_enabled() {
                for locale in &locale_config.locales {
                    field_names.insert(format!("{}__{}", f.name, locale));
                }
            } else {
                field_names.insert(f.name.clone());
            }
        }
    }
    let system_columns: HashSet<&str> = [
        "id",
        "created_at",
        "updated_at",
        "_password_hash",
        "_reset_token",
        "_reset_token_exp",
        "_verified",
        "_verification_token",
        "_verification_token_exp",
        "_locked",
        "_status",
        "_settings",
    ]
    .into();
    for col in &existing_columns {
        if !field_names.contains(col) && !system_columns.contains(col.as_str()) {
            tracing::warn!(
                "Column '{}' exists in table '{}' but not in Lua definition (not removed)",
                col,
                slug
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::create::create_collection_table;
    use super::super::test_helpers::*;
    use super::*;
    use crate::core::collection::*;
    use crate::core::field::{FieldDefinition, FieldTab, FieldType};
    use crate::db::migrate::helpers::get_table_columns;

    #[test]
    fn alter_adds_new_column() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection("posts", vec![text_field("title"), text_field("summary")]);
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("summary"), "new column should be added");
    }

    #[test]
    fn alter_adds_auth_system_columns() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("users", vec![text_field("email")]);
        create_collection_table(&conn, "users", &def1, &no_locale()).unwrap();

        // Now make it an auth collection with verify_email
        let mut def2 = simple_collection("users", vec![text_field("email")]);
        def2.auth = Some(Auth {
            enabled: true,
            verify_email: true,
            ..Default::default()
        });
        alter_collection_table(&conn, "users", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "users").unwrap();
        assert!(cols.contains("_password_hash"));
        assert!(cols.contains("_reset_token"));
        assert!(cols.contains("_reset_token_exp"));
        assert!(cols.contains("_locked"));
        assert!(cols.contains("_settings"));
        assert!(cols.contains("_verified"));
        assert!(cols.contains("_verification_token"));
    }

    #[test]
    fn alter_adds_status_for_drafts() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        // Enable drafts on existing collection
        let mut def2 = simple_collection("posts", vec![text_field("title")]);
        def2.versions = Some(VersionsConfig::new(true, 5));
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("_status"));
    }

    #[test]
    fn alter_adds_timestamps() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        // Create a table without timestamps
        conn.execute("CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT)", [])
            .unwrap();

        let def = simple_collection("posts", vec![text_field("title")]);
        alter_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("created_at"));
        assert!(cols.contains("updated_at"));
    }

    #[test]
    fn alter_collection_with_localized_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![localized_field("title")]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();

        // Add a new localized field via alter
        let def2 = simple_collection(
            "posts",
            vec![localized_field("title"), localized_field("body")],
        );
        alter_collection_table(&conn, "posts", &def2, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("body__en"));
        assert!(cols.contains("body__de"));
    }

    #[test]
    fn alter_adds_group_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("seo", FieldType::Group)
                    .fields(vec![text_field("meta_title"), text_field("meta_desc")])
                    .build(),
            ],
        );
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__meta_title"));
        assert!(cols.contains("seo__meta_desc"));
    }

    #[test]
    fn alter_adds_localized_group_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &locale_en_de()).unwrap();

        let def2 = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("seo", FieldType::Group)
                    .localized(true)
                    .fields(vec![text_field("meta_title")])
                    .build(),
            ],
        );
        alter_collection_table(&conn, "posts", &def2, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__meta_title__en"));
        assert!(cols.contains("seo__meta_title__de"));
    }

    #[test]
    fn alter_adds_row_sub_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("names", FieldType::Row)
                    .fields(vec![text_field("first"), text_field("last")])
                    .build(),
            ],
        );
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("first"));
        assert!(cols.contains("last"));
    }

    #[test]
    fn alter_adds_collapsible_sub_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("extra", FieldType::Collapsible)
                    .fields(vec![text_field("notes")])
                    .build(),
            ],
        );
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("notes"));
    }

    #[test]
    fn alter_adds_tabs_sub_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("tabs", FieldType::Tabs)
                    .tabs(vec![FieldTab::new("T1", vec![text_field("body")])])
                    .build(),
            ],
        );
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("body"));
    }

    #[test]
    fn alter_adds_tabs_with_group_sub_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def1 = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def1, &no_locale()).unwrap();

        let def2 = simple_collection(
            "posts",
            vec![
                text_field("title"),
                FieldDefinition::builder("tabs", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "SEO",
                        vec![
                            FieldDefinition::builder("seo", FieldType::Group)
                                .fields(vec![text_field("og_title"), text_field("og_desc")])
                                .build(),
                        ],
                    )])
                    .build(),
            ],
        );
        alter_collection_table(&conn, "posts", &def2, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(
            cols.contains("seo__og_title"),
            "ALTER should add Group columns inside Tabs"
        );
        assert!(
            cols.contains("seo__og_desc"),
            "ALTER should add Group columns inside Tabs"
        );
    }
}
