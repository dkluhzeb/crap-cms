//! Global document query functions.

use anyhow::{Context as _, Result};
use rusqlite::params_from_iter;
use std::collections::HashMap;

use super::{coerce_value, group_locale_fields, locale_write_column, LocaleContext, LocaleMode};
use crate::core::collection::GlobalDefinition;
use crate::core::field::FieldType;
use crate::core::Document;
use crate::db::document::row_to_document;

/// Get the single global document from `_global_{slug}`.
pub fn get_global(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &GlobalDefinition,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let table_name = format!("_global_{}", slug);

    let (select_exprs, result_names) = match locale_ctx {
        Some(ctx) if ctx.config.is_enabled() => get_global_locale_columns(def, ctx),
        _ => {
            let names = get_global_column_names(def);
            (names.clone(), names)
        }
    };

    let sql = format!(
        "SELECT {} FROM {} WHERE id = 'default'",
        select_exprs.join(", "),
        table_name
    );

    let mut doc = conn
        .query_row(&sql, [], |row| row_to_document(row, &result_names))
        .with_context(|| format!("Failed to get global '{}'", slug))?;

    if let Some(ctx) = locale_ctx {
        if ctx.config.is_enabled() {
            if let LocaleMode::All = ctx.mode {
                group_locale_fields(&mut doc, &def.fields, &ctx.config);
            }
        }
    }

    // Hydrate join table data (arrays, blocks, has-many relationships)
    super::hydrate_document(conn, &table_name, &def.fields, &mut doc, None, locale_ctx)?;

    Ok(doc)
}

/// Update the single global document in `_global_{slug}`. Returns the updated document.
pub fn update_global(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &GlobalDefinition,
    data: &HashMap<String, String>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let table_name = format!("_global_{}", slug);
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S.000Z")
        .to_string();

    let mut set_clauses = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut idx = 1;

    collect_update_params(
        &def.fields,
        data,
        &locale_ctx,
        &mut set_clauses,
        &mut params,
        &mut idx,
        "",
    );

    set_clauses.push(format!("updated_at = ?{}", idx));
    params.push(Box::new(now));

    if set_clauses.is_empty() {
        return get_global(conn, slug, def, locale_ctx);
    }

    let sql = format!(
        "UPDATE {} SET {} WHERE id = 'default'",
        table_name,
        set_clauses.join(", ")
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    conn.execute(&sql, params_from_iter(param_refs.iter()))
        .with_context(|| format!("Failed to update global '{}'", slug))?;

    get_global(conn, slug, def, locale_ctx)
}

/// Recursively collect SET clauses + params from a field list.
/// Handles arbitrary nesting: Group (prefixed), Row/Collapsible/Tabs (promoted flat).
fn collect_update_params(
    fields: &[crate::core::field::FieldDefinition],
    data: &HashMap<String, String>,
    locale_ctx: &Option<&LocaleContext>,
    set_clauses: &mut Vec<String>,
    params: &mut Vec<Box<dyn rusqlite::types::ToSql>>,
    idx: &mut usize,
    prefix: &str,
) {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                collect_update_params(
                    &field.fields,
                    data,
                    locale_ctx,
                    set_clauses,
                    params,
                    idx,
                    &new_prefix,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_update_params(
                    &field.fields,
                    data,
                    locale_ctx,
                    set_clauses,
                    params,
                    idx,
                    prefix,
                );
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_update_params(
                        &tab.fields,
                        data,
                        locale_ctx,
                        set_clauses,
                        params,
                        idx,
                        prefix,
                    );
                }
            }
            _ => {
                if !field.has_parent_column() {
                    continue;
                }
                let data_key = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                let col_name = locale_write_column(&data_key, field, locale_ctx);
                if let Some(value) = data.get(&data_key) {
                    set_clauses.push(format!("{} = ?{}", col_name, *idx));
                    params.push(coerce_value(&field.field_type, value));
                    *idx += 1;
                } else if field.field_type == FieldType::Checkbox {
                    set_clauses.push(format!("{} = ?{}", col_name, *idx));
                    params.push(Box::new(0i32));
                    *idx += 1;
                }
            }
        }
    }
}

fn get_global_column_names(def: &GlobalDefinition) -> Vec<String> {
    let mut names = vec!["id".to_string()];
    super::collect_column_names(&def.fields, &mut names);
    if def.has_drafts() {
        names.push("_status".to_string());
    }
    names.push("created_at".to_string());
    names.push("updated_at".to_string());
    names
}

/// Build SELECT columns for globals with locale support.
/// Uses the shared recursive logic from mod.rs.
fn get_global_locale_columns(
    def: &GlobalDefinition,
    ctx: &LocaleContext,
) -> (Vec<String>, Vec<String>) {
    let (mut select_exprs, mut result_names) =
        super::get_locale_select_columns(&def.fields, true, ctx);

    // Insert _status before timestamps if present
    if def.has_drafts() {
        let ts_pos = select_exprs.len() - 2;
        select_exprs.insert(ts_pos, "_status".to_string());
        result_names.insert(ts_pos, "_status".to_string());
    }

    (select_exprs, result_names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;
    use crate::core::field::*;
    use rusqlite::Connection;

    fn global_def() -> GlobalDefinition {
        let mut def = GlobalDefinition::new("settings");
        def.fields = vec![
            FieldDefinition::builder("site_name", FieldType::Text).build(),
            FieldDefinition::builder("tagline", FieldType::Text).build(),
        ];
        def
    }

    fn setup_global_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE _global_settings (
                id TEXT PRIMARY KEY,
                site_name TEXT,
                tagline TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO _global_settings (id, site_name, tagline, created_at, updated_at)
            VALUES ('default', NULL, NULL, '2024-01-01', '2024-01-01');",
        )
        .unwrap();
        conn
    }

    /// Globals with group fields expand sub-fields into `field__subfield` columns
    /// (same as collections).
    #[test]
    fn get_global_with_group_fields_and_locale() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE _global_site (
                id TEXT PRIMARY KEY,
                site_name__en TEXT,
                site_name__de TEXT,
                social__github TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO _global_site (id, site_name__en, social__github, created_at, updated_at)
            VALUES ('default', 'My Site', 'https://github.com', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let mut def = GlobalDefinition::new("site");
        def.fields = vec![
            FieldDefinition::builder("site_name", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("social", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("github", FieldType::Text).build()
                ])
                .build(),
        ];
        let def = def;

        let locale_config = crate::config::LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        let locale_ctx = LocaleContext::from_locale_string(Some("en"), &locale_config);

        let doc = get_global(&conn, "site", &def, locale_ctx.as_ref()).unwrap();
        assert_eq!(doc.id, "default");
        assert_eq!(doc.get_str("site_name"), Some("My Site"));
        // Group sub-field reconstructed as nested object by hydrate_document
        let social = doc.fields.get("social").expect("social should exist");
        assert_eq!(
            social.get("github").and_then(|v| v.as_str()),
            Some("https://github.com")
        );
    }

    #[test]
    fn get_global_default_row() {
        let conn = setup_global_db();
        let def = global_def();
        let doc = get_global(&conn, "settings", &def, None).unwrap();
        assert_eq!(doc.id, "default");
        assert!(doc.created_at.is_some(), "created_at should be present");
        assert!(doc.updated_at.is_some(), "updated_at should be present");
        assert_eq!(doc.created_at.as_deref(), Some("2024-01-01"));
        assert_eq!(doc.updated_at.as_deref(), Some("2024-01-01"));
    }

    #[test]
    fn update_global_sets_field() {
        let conn = setup_global_db();
        let def = global_def();
        let mut data = HashMap::new();
        data.insert("site_name".to_string(), "My Site".to_string());
        let doc = update_global(&conn, "settings", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("site_name"), Some("My Site"));
    }

    #[test]
    fn update_global_preserves_unset() {
        let conn = setup_global_db();
        let def = global_def();

        // First update: set site_name
        let mut data1 = HashMap::new();
        data1.insert("site_name".to_string(), "My Site".to_string());
        update_global(&conn, "settings", &def, &data1, None).unwrap();

        // Second update: set only tagline
        let mut data2 = HashMap::new();
        data2.insert("tagline".to_string(), "A great site".to_string());
        let doc = update_global(&conn, "settings", &def, &data2, None).unwrap();

        assert_eq!(
            doc.get_str("site_name"),
            Some("My Site"),
            "site_name should be preserved"
        );
        assert_eq!(
            doc.get_str("tagline"),
            Some("A great site"),
            "tagline should be set"
        );
    }

    #[test]
    fn update_global_updates_timestamp() {
        let conn = setup_global_db();
        let def = global_def();

        let before = get_global(&conn, "settings", &def, None).unwrap();
        assert_eq!(before.updated_at.as_deref(), Some("2024-01-01"));

        let mut data = HashMap::new();
        data.insert("site_name".to_string(), "New Name".to_string());
        let after = update_global(&conn, "settings", &def, &data, None).unwrap();

        assert_ne!(
            after.updated_at.as_deref(),
            Some("2024-01-01"),
            "updated_at should have changed after update"
        );
        assert!(after.updated_at.is_some(), "updated_at should be present");
    }

    #[test]
    fn update_global_checkbox_defaults_to_zero() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE _global_prefs (
                id TEXT PRIMARY KEY,
                newsletter INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO _global_prefs (id, newsletter, created_at, updated_at)
            VALUES ('default', 1, '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let mut def = GlobalDefinition::new("prefs");
        def.fields = vec![FieldDefinition::builder("newsletter", FieldType::Checkbox).build()];
        let def = def;

        // Update without providing the checkbox field -- should default to 0
        let data = HashMap::new();
        let doc = update_global(&conn, "prefs", &def, &data, None).unwrap();
        assert_eq!(doc.get("newsletter"), Some(&serde_json::json!(0)));
    }

    #[test]
    fn update_global_group_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE _global_branding (
                id TEXT PRIMARY KEY,
                colors__primary TEXT,
                colors__secondary TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO _global_branding (id, created_at, updated_at)
            VALUES ('default', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let mut def = GlobalDefinition::new("branding");
        def.fields = vec![FieldDefinition::builder("colors", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("primary", FieldType::Text).build(),
                FieldDefinition::builder("secondary", FieldType::Text).build(),
            ])
            .build()];
        let def = def;

        let mut data = HashMap::new();
        data.insert("colors__primary".to_string(), "#ff0000".to_string());
        data.insert("colors__secondary".to_string(), "#00ff00".to_string());

        let doc = update_global(&conn, "branding", &def, &data, None).unwrap();
        // Group fields should be reconstructed as nested object by hydrate_document
        let colors = doc.fields.get("colors").expect("colors should exist");
        assert_eq!(
            colors.get("primary").and_then(|v| v.as_str()),
            Some("#ff0000")
        );
        assert_eq!(
            colors.get("secondary").and_then(|v| v.as_str()),
            Some("#00ff00")
        );
    }

    #[test]
    fn get_global_column_names_with_drafts() {
        let mut def = GlobalDefinition::new("settings");
        def.fields = vec![FieldDefinition::builder("site_name", FieldType::Text).build()];
        def.versions = Some(VersionsConfig::new(true, 10));
        let def = def;

        let names = get_global_column_names(&def);
        assert!(
            names.contains(&"_status".to_string()),
            "should include _status for drafts-enabled global"
        );
        assert!(names.contains(&"created_at".to_string()));
        assert!(names.contains(&"updated_at".to_string()));
    }

    // ── Group containing layout fields (the former terminal-node bug) ─────

    #[test]
    fn update_global_group_containing_row() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE _global_branding (
                id TEXT PRIMARY KEY,
                colors__primary TEXT,
                colors__secondary TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO _global_branding (id, created_at, updated_at)
            VALUES ('default', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let mut def = GlobalDefinition::new("branding");
        def.fields = vec![FieldDefinition::builder("colors", FieldType::Group)
            .fields(vec![FieldDefinition::builder("r", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("primary", FieldType::Text).build(),
                    FieldDefinition::builder("secondary", FieldType::Text).build(),
                ])
                .build()])
            .build()];
        let def = def;

        let mut data = HashMap::new();
        data.insert("colors__primary".to_string(), "#ff0000".to_string());
        data.insert("colors__secondary".to_string(), "#00ff00".to_string());

        let doc = update_global(&conn, "branding", &def, &data, None).unwrap();
        let colors = doc.fields.get("colors").expect("colors should exist");
        assert_eq!(
            colors.get("primary").and_then(|v| v.as_str()),
            Some("#ff0000")
        );
        assert_eq!(
            colors.get("secondary").and_then(|v| v.as_str()),
            Some("#00ff00")
        );
    }

    #[test]
    fn update_global_group_containing_tabs() {
        use crate::core::field::FieldTab;
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE _global_settings (
                id TEXT PRIMARY KEY,
                config__theme TEXT,
                config__cache_ttl TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO _global_settings (id, created_at, updated_at)
            VALUES ('default', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let mut def = GlobalDefinition::new("settings");
        def.fields = vec![FieldDefinition::builder("config", FieldType::Group)
            .fields(vec![FieldDefinition::builder("t", FieldType::Tabs)
                .tabs(vec![
                    FieldTab::new(
                        "General",
                        vec![FieldDefinition::builder("theme", FieldType::Text).build()],
                    ),
                    FieldTab::new(
                        "Perf",
                        vec![FieldDefinition::builder("cache_ttl", FieldType::Text).build()],
                    ),
                ])
                .build()])
            .build()];
        let def = def;

        let mut data = HashMap::new();
        data.insert("config__theme".to_string(), "dark".to_string());
        data.insert("config__cache_ttl".to_string(), "3600".to_string());

        let doc = update_global(&conn, "settings", &def, &data, None).unwrap();
        let config = doc.fields.get("config").expect("config should exist");
        assert_eq!(config.get("theme").and_then(|v| v.as_str()), Some("dark"));
        assert_eq!(
            config.get("cache_ttl").and_then(|v| v.as_str()),
            Some("3600")
        );
    }

    #[test]
    fn get_global_column_names_group_containing_tabs() {
        use crate::core::field::FieldTab;
        let mut def = GlobalDefinition::new("settings");
        def.fields = vec![FieldDefinition::builder("config", FieldType::Group)
            .fields(vec![FieldDefinition::builder("t", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Tab",
                    vec![FieldDefinition::builder("value", FieldType::Text).build()],
                )])
                .build()])
            .build()];
        let def = def;

        let names = get_global_column_names(&def);
        assert!(
            names.contains(&"config__value".to_string()),
            "Group→Tabs: config__value"
        );
    }
}
