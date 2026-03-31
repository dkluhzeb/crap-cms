//! Collection table creation from Lua definitions.

use anyhow::{Context as _, Result};
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, FieldType},
    db::{
        DbConnection,
        migrate::helpers::{collect_column_specs, sanitize_locale},
    },
};

pub fn create_collection_table(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];

    for spec in &collect_column_specs(&def.fields, locale_config) {
        if spec.is_localized {
            for locale in &locale_config.locales {
                let col_name = format!("{}__{}", spec.col_name, sanitize_locale(locale)?);
                let col_type = if spec.companion_text {
                    "TEXT"
                } else {
                    conn.column_type_for(&spec.field.field_type)
                };
                let mut col = format!("{} {}", col_name, col_type);

                if !spec.companion_text {
                    if spec.field.required
                        && *locale == locale_config.default_locale
                        && !def.has_drafts()
                    {
                        col.push_str(" NOT NULL");
                    }
                    // Skip inline UNIQUE for soft-delete collections — a partial
                    // unique index (WHERE _deleted_at IS NULL) is created instead
                    // by sync_indexes so that deleted rows don't block new inserts.
                    if spec.field.unique && !def.soft_delete {
                        col.push_str(" UNIQUE");
                    }
                    append_default_value_for(
                        &mut col,
                        &spec.field.default_value,
                        &spec.field.field_type,
                        conn.kind(),
                    );
                }
                columns.push(col);
            }
        } else {
            let col_type = if spec.companion_text {
                "TEXT"
            } else {
                conn.column_type_for(&spec.field.field_type)
            };
            let mut col = format!("{} {}", spec.col_name, col_type);

            if !spec.companion_text {
                if spec.field.required && !def.has_drafts() {
                    col.push_str(" NOT NULL");
                }
                // Skip inline UNIQUE for soft-delete collections (see above).
                if spec.field.unique && !def.soft_delete {
                    col.push_str(" UNIQUE");
                }
                append_default_value_for(
                    &mut col,
                    &spec.field.default_value,
                    &spec.field.field_type,
                    conn.kind(),
                );
            }
            columns.push(col);
        }
    }

    // Versioned collections with drafts get a _status column
    if def.has_drafts() {
        columns.push("_status TEXT NOT NULL DEFAULT 'published'".to_string());
    }

    // Soft-delete collections get a _deleted_at column
    if def.soft_delete {
        columns.push(format!("_deleted_at {}", conn.timestamp_column_type()));
    }

    // All collections get a reference count for delete protection
    columns.push("_ref_count INTEGER NOT NULL DEFAULT 0".to_string());

    // Auth collections get hidden system columns (not regular fields)
    if def.is_auth_collection() {
        columns.push("_password_hash TEXT".to_string());
        columns.push("_reset_token TEXT".to_string());
        columns.push("_reset_token_exp INTEGER".to_string());
        columns.push("_locked INTEGER DEFAULT 0".to_string());
        columns.push("_settings TEXT".to_string());
        columns.push("_session_version INTEGER DEFAULT 0".to_string());

        if def.auth.as_ref().is_some_and(|a| a.verify_email) {
            columns.push("_verified INTEGER DEFAULT 0".to_string());
            columns.push("_verification_token TEXT".to_string());
            columns.push("_verification_token_exp INTEGER".to_string());
        }
    }

    if def.timestamps {
        columns.push(format!("created_at {}", conn.timestamp_column_default()));
        columns.push(format!("updated_at {}", conn.timestamp_column_default()));
    }

    let sql = format!("CREATE TABLE \"{}\" ({})", slug, columns.join(", "));

    tracing::info!("Creating collection table: {}", slug);
    tracing::debug!("SQL: {}", sql);

    conn.execute_ddl(&sql, &[])
        .with_context(|| format!("Failed to create table {}", slug))?;

    Ok(())
}

/// Append a DEFAULT value clause to a column definition string.
#[cfg(test)]
pub fn append_default_value(
    col: &mut String,
    default_value: &Option<Value>,
    field_type: &FieldType,
) {
    append_default_value_for(col, default_value, field_type, "sqlite");
}

/// Append a DEFAULT clause. Uses `0`/`1` for booleans (INTEGER on all backends).
pub fn append_default_value_for(
    col: &mut String,
    default_value: &Option<Value>,
    field_type: &FieldType,
    _db_kind: &str,
) {
    if let Some(default) = &default_value {
        warn_default_type_mismatch(default, field_type);

        match default {
            Value::String(s) => col.push_str(&format!(" DEFAULT '{}'", s.replace('\'', "''"))),
            Value::Number(n) => col.push_str(&format!(" DEFAULT {}", n)),
            Value::Bool(b) => col.push_str(&format!(" DEFAULT {}", if *b { 1 } else { 0 })),
            _ => {}
        }
    } else if *field_type == FieldType::Checkbox {
        col.push_str(" DEFAULT 0");
    }
}

/// Log a warning when a default value type obviously mismatches the field type.
fn warn_default_type_mismatch(default: &Value, field_type: &FieldType) {
    match (default, field_type) {
        (Value::String(_), FieldType::Number | FieldType::Checkbox) => {
            tracing::warn!(
                "String default value on {:?} field — possible type mismatch",
                field_type
            );
        }
        (Value::Bool(_), FieldType::Text | FieldType::Textarea | FieldType::Email) => {
            tracing::warn!(
                "Bool default value on {:?} field — possible type mismatch",
                field_type
            );
        }
        (Value::Number(_), FieldType::Checkbox) => {
            tracing::warn!("Number default value on Checkbox field — use a bool default instead");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::test_helpers::*;
    use super::*;
    use crate::core::collection::*;
    use crate::core::field::{FieldDefinition, FieldTab, FieldType};
    use crate::db::migrate::helpers::{get_table_columns, table_exists};

    #[test]
    fn create_simple_collection_table() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![text_field("title"), text_field("body")]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        assert!(table_exists(&conn, "posts").unwrap());
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("id"));
        assert!(cols.contains("title"));
        assert!(cols.contains("body"));
        assert!(cols.contains("created_at"));
        assert!(cols.contains("updated_at"));
    }

    #[test]
    fn create_with_localized_fields() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![localized_field("title")]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("title__en"), "should have en locale column");
        assert!(cols.contains("title__de"), "should have de locale column");
        assert!(!cols.contains("title"), "should NOT have bare title column");
    }

    #[test]
    fn create_auth_collection_has_system_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("users", vec![text_field("email")]);
        def.auth = Some({
            let mut auth = Auth::new(true);
            auth.verify_email = true;
            auth
        });
        create_collection_table(&conn, "users", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "users").unwrap();
        assert!(cols.contains("_password_hash"));
        assert!(cols.contains("_reset_token"));
        assert!(cols.contains("_reset_token_exp"));
        assert!(cols.contains("_locked"));
        assert!(cols.contains("_settings"));
        assert!(cols.contains("_session_version"));
        assert!(cols.contains("_verified"));
        assert!(cols.contains("_verification_token"));
    }

    #[test]
    fn drafts_collection_has_status_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.versions = Some(VersionsConfig::new(true, 0));
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("_status"));
    }

    #[test]
    fn group_field_creates_prefixed_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("seo", FieldType::Group)
                    .fields(vec![text_field("meta_title"), text_field("meta_desc")])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__meta_title"));
        assert!(cols.contains("seo__meta_desc"));
        assert!(
            !cols.contains("seo"),
            "group field itself should not be a column"
        );
    }

    #[test]
    fn create_with_default_values() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("status", FieldType::Text)
                    .default_value(json!("draft"))
                    .build(),
                FieldDefinition::builder("count", FieldType::Number)
                    .default_value(json!(0))
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        // Just verify it was created (defaults encoded in DDL)
        assert!(table_exists(&conn, "posts").unwrap());
    }

    #[test]
    fn create_with_required_unique_fields() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .required(true)
                    .unique(true)
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        assert!(table_exists(&conn, "posts").unwrap());
    }

    #[test]
    fn create_collection_no_timestamps() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.timestamps = false;
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(!cols.contains("created_at"));
        assert!(!cols.contains("updated_at"));
    }

    #[test]
    fn create_localized_group_subfield() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("seo", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("title", FieldType::Text)
                            .localized(true)
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    #[test]
    fn create_required_localized_field() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("title", FieldType::Text)
                    .localized(true)
                    .required(true)
                    .unique(true)
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();

        // Should succeed — NOT NULL only on default locale
        assert!(table_exists(&conn, "posts").unwrap());
    }

    #[test]
    fn create_required_localized_group_subfield() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("seo", FieldType::Group)
                    .localized(true)
                    .fields(vec![
                        FieldDefinition::builder("title", FieldType::Text)
                            .required(true)
                            .unique(true)
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    #[test]
    fn row_field_promotes_sub_fields_without_prefix() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("layout", FieldType::Row)
                    .fields(vec![text_field("first_name"), text_field("last_name")])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(
            cols.contains("first_name"),
            "Row sub-field should be a top-level column"
        );
        assert!(
            cols.contains("last_name"),
            "Row sub-field should be a top-level column"
        );
        assert!(
            !cols.contains("layout"),
            "Row field itself should not be a column"
        );
        assert!(
            !cols.contains("layout__first_name"),
            "Row should not use prefix"
        );
    }

    #[test]
    fn collapsible_field_promotes_sub_fields_without_prefix() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("details", FieldType::Collapsible)
                    .fields(vec![text_field("summary"), text_field("notes")])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(
            cols.contains("summary"),
            "Collapsible sub-field should be promoted"
        );
        assert!(
            cols.contains("notes"),
            "Collapsible sub-field should be promoted"
        );
        assert!(
            !cols.contains("details"),
            "Collapsible container should not be a column"
        );
        assert!(
            !cols.contains("details__summary"),
            "Collapsible should not use prefix"
        );
    }

    #[test]
    fn tabs_field_promotes_sub_fields_without_prefix() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![
                        FieldTab::new("Content", vec![text_field("body")]),
                        FieldTab::new("SEO", vec![text_field("meta_title")]),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("body"), "Tabs sub-field should be promoted");
        assert!(
            cols.contains("meta_title"),
            "Tabs sub-field should be promoted"
        );
        assert!(
            !cols.contains("layout"),
            "Tabs container should not be a column"
        );
    }

    #[test]
    fn tabs_containing_group_creates_prefixed_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![
                        FieldTab::new(
                            "Social",
                            vec![
                                FieldDefinition::builder("social", FieldType::Group)
                                    .fields(vec![text_field("github"), text_field("twitter")])
                                    .build(),
                            ],
                        ),
                        FieldTab::new("Content", vec![text_field("body")]),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(
            cols.contains("social__github"),
            "Group inside Tabs should use group__subfield"
        );
        assert!(
            cols.contains("social__twitter"),
            "Group inside Tabs should use group__subfield"
        );
        assert!(
            cols.contains("body"),
            "Plain field in Tabs should be promoted flat"
        );
        assert!(
            !cols.contains("social"),
            "Group itself should not be a column"
        );
    }

    #[test]
    fn collapsible_containing_group_creates_prefixed_columns() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
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
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(
            cols.contains("seo__title"),
            "Group inside Collapsible should use group__subfield"
        );
        assert!(
            cols.contains("seo__desc"),
            "Group inside Collapsible should use group__subfield"
        );
        assert!(!cols.contains("seo"), "Group itself should not be a column");
    }

    #[test]
    fn deeply_nested_tabs_collapsible_group() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "Advanced",
                        vec![
                            FieldDefinition::builder("advanced", FieldType::Collapsible)
                                .fields(vec![
                                    FieldDefinition::builder("og", FieldType::Group)
                                        .fields(vec![text_field("image"), text_field("title")])
                                        .build(),
                                    text_field("canonical"),
                                ])
                                .build(),
                        ],
                    )])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(
            cols.contains("og__image"),
            "Deeply nested Group inside Collapsible inside Tabs"
        );
        assert!(
            cols.contains("og__title"),
            "Deeply nested Group inside Collapsible inside Tabs"
        );
        assert!(
            cols.contains("canonical"),
            "Plain field in Collapsible inside Tabs"
        );
    }

    #[test]
    fn group_containing_row() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("row1", FieldType::Row)
                            .fields(vec![text_field("title"), text_field("slug")])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(
            cols.contains("meta__title"),
            "Group→Row should produce meta__title"
        );
        assert!(
            cols.contains("meta__slug"),
            "Group→Row should produce meta__slug"
        );
    }

    #[test]
    fn group_containing_collapsible() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("seo", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("advanced", FieldType::Collapsible)
                            .fields(vec![text_field("robots"), text_field("canonical")])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(
            cols.contains("seo__robots"),
            "Group→Collapsible should produce seo__robots"
        );
        assert!(
            cols.contains("seo__canonical"),
            "Group→Collapsible should produce seo__canonical"
        );
    }

    #[test]
    fn group_containing_tabs() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("settings", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("layout", FieldType::Tabs)
                            .tabs(vec![
                                FieldTab::new("General", vec![text_field("theme")]),
                                FieldTab::new("Advanced", vec![text_field("cache_ttl")]),
                            ])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(
            cols.contains("settings__theme"),
            "Group→Tabs should produce settings__theme"
        );
        assert!(
            cols.contains("settings__cache_ttl"),
            "Group→Tabs should produce settings__cache_ttl"
        );
    }

    #[test]
    fn group_tabs_group_three_levels() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("outer", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("layout", FieldType::Tabs)
                            .tabs(vec![FieldTab::new(
                                "Nested",
                                vec![
                                    FieldDefinition::builder("inner", FieldType::Group)
                                        .fields(vec![text_field("deep_value")])
                                        .build(),
                                ],
                            )])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(
            cols.contains("outer__inner__deep_value"),
            "Group→Tabs→Group should produce outer__inner__deep_value"
        );
    }

    #[test]
    fn group_row_group_collapsible_four_levels() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("a", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("r", FieldType::Row)
                            .fields(vec![
                                FieldDefinition::builder("b", FieldType::Group)
                                    .fields(vec![
                                        FieldDefinition::builder("c", FieldType::Collapsible)
                                            .fields(vec![text_field("leaf")])
                                            .build(),
                                    ])
                                    .build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(
            cols.contains("a__b__leaf"),
            "Group→Row→Group→Collapsible: a__b__leaf"
        );
    }

    #[test]
    fn group_containing_tabs_with_locale() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .localized(true)
                    .fields(vec![
                        FieldDefinition::builder("layout", FieldType::Tabs)
                            .tabs(vec![FieldTab::new("Content", vec![text_field("title")])])
                            .build(),
                    ])
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(
            cols.contains("meta__title__en"),
            "Localized Group→Tabs: meta__title__en"
        );
        assert!(
            cols.contains("meta__title__de"),
            "Localized Group→Tabs: meta__title__de"
        );
    }

    #[test]
    fn append_default_string() {
        let mut col = "name TEXT".to_string();
        append_default_value(&mut col, &Some(json!("hello")), &FieldType::Text);
        assert!(col.contains("DEFAULT 'hello'"));
    }

    #[test]
    fn append_default_number() {
        let mut col = "count REAL".to_string();
        append_default_value(&mut col, &Some(json!(42)), &FieldType::Number);
        assert!(col.contains("DEFAULT 42"));
    }

    #[test]
    fn append_default_bool() {
        let mut col = "active INTEGER".to_string();
        append_default_value(&mut col, &Some(json!(true)), &FieldType::Checkbox);
        assert!(col.contains("DEFAULT 1"));
    }

    #[test]
    fn append_default_checkbox_none() {
        let mut col = "active INTEGER".to_string();
        append_default_value(&mut col, &None, &FieldType::Checkbox);
        assert!(col.contains("DEFAULT 0"));
    }

    #[test]
    fn append_default_none_non_checkbox() {
        let mut col = "name TEXT".to_string();
        append_default_value(&mut col, &None, &FieldType::Text);
        assert!(!col.contains("DEFAULT"));
    }

    #[test]
    fn soft_delete_collection_has_deleted_at_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.soft_delete = true;
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("_deleted_at"));
    }

    #[test]
    fn non_soft_delete_collection_has_no_deleted_at_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![text_field("title")]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(!cols.contains("_deleted_at"));
    }

    #[test]
    fn create_date_field_with_timezone_creates_tz_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "events",
            vec![
                FieldDefinition::builder("starts_at", FieldType::Date)
                    .timezone(true)
                    .build(),
            ],
        );
        create_collection_table(&conn, "events", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "events").unwrap();
        assert!(cols.contains("starts_at"), "should have main date column");
        assert!(
            cols.contains("starts_at_tz"),
            "should have companion timezone column"
        );
    }

    #[test]
    fn create_date_field_without_timezone_has_no_tz_column() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "events",
            vec![FieldDefinition::builder("starts_at", FieldType::Date).build()],
        );
        create_collection_table(&conn, "events", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "events").unwrap();
        assert!(cols.contains("starts_at"));
        assert!(
            !cols.contains("starts_at_tz"),
            "should NOT have timezone column when timezone is false"
        );
    }

    #[test]
    fn create_localized_date_with_timezone() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "events",
            vec![
                FieldDefinition::builder("starts_at", FieldType::Date)
                    .timezone(true)
                    .localized(true)
                    .build(),
            ],
        );
        create_collection_table(&conn, "events", &def, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "events").unwrap();
        assert!(cols.contains("starts_at__en"));
        assert!(cols.contains("starts_at__de"));
        assert!(cols.contains("starts_at_tz__en"));
        assert!(cols.contains("starts_at_tz__de"));
    }

    #[test]
    fn soft_delete_unique_field_skips_inline_unique() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .unique(true)
                    .build(),
            ],
        );
        def.soft_delete = true;
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        // Insert two rows with the same slug — inline UNIQUE would block this,
        // but we skipped it for soft-delete collections.
        conn.execute(
            "INSERT INTO posts (id, slug, _deleted_at) VALUES ('a', 'hello', '2025-01-01')",
            &[],
        )
        .unwrap();
        let result = conn.execute(
            "INSERT INTO posts (id, slug, _deleted_at) VALUES ('b', 'hello', NULL)",
            &[],
        );
        assert!(
            result.is_ok(),
            "Should allow duplicate slug when one row is soft-deleted"
        );
    }

    #[test]
    fn non_soft_delete_unique_field_keeps_inline_unique() {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .unique(true)
                    .build(),
            ],
        );
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        conn.execute("INSERT INTO posts (id, slug) VALUES ('a', 'hello')", &[])
            .unwrap();
        let result = conn.execute("INSERT INTO posts (id, slug) VALUES ('b', 'hello')", &[]);
        assert!(
            result.is_err(),
            "Inline UNIQUE should block duplicate slug on non-soft-delete collection"
        );
    }
}
