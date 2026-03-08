//! Collection table creation from Lua definitions.

use anyhow::{Context as _, Result};

use crate::config::LocaleConfig;
use crate::core::field::FieldType;

pub fn create_collection_table(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &crate::core::CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];

    for spec in &crate::db::migrate::helpers::collect_column_specs(&def.fields, locale_config) {
        if spec.is_localized {
            for locale in &locale_config.locales {
                let col_name = format!("{}__{}", spec.col_name, locale);
                let mut col = format!("{} {}", col_name, spec.field.field_type.sqlite_type());
                if spec.field.required && *locale == locale_config.default_locale && !def.has_drafts() {
                    col.push_str(" NOT NULL");
                }
                if spec.field.unique {
                    col.push_str(" UNIQUE");
                }
                append_default_value(&mut col, &spec.field.default_value, &spec.field.field_type);
                columns.push(col);
            }
        } else {
            let mut col = format!("{} {}", spec.col_name, spec.field.field_type.sqlite_type());
            if spec.field.required && !def.has_drafts() {
                col.push_str(" NOT NULL");
            }
            if spec.field.unique {
                col.push_str(" UNIQUE");
            }
            append_default_value(&mut col, &spec.field.default_value, &spec.field.field_type);
            columns.push(col);
        }
    }

    // Versioned collections with drafts get a _status column
    if def.has_drafts() {
        columns.push("_status TEXT NOT NULL DEFAULT 'published'".to_string());
    }

    // Auth collections get hidden system columns (not regular fields)
    if def.is_auth_collection() {
        columns.push("_password_hash TEXT".to_string());
        columns.push("_reset_token TEXT".to_string());
        columns.push("_reset_token_exp INTEGER".to_string());
        columns.push("_locked INTEGER DEFAULT 0".to_string());
        columns.push("_settings TEXT".to_string());
        if def.auth.as_ref().is_some_and(|a| a.verify_email) {
            columns.push("_verified INTEGER DEFAULT 0".to_string());
            columns.push("_verification_token TEXT".to_string());
            columns.push("_verification_token_exp INTEGER".to_string());
        }
    }

    if def.timestamps {
        columns.push("created_at TEXT DEFAULT (datetime('now'))".to_string());
        columns.push("updated_at TEXT DEFAULT (datetime('now'))".to_string());
    }

    let sql = format!("CREATE TABLE {} ({})", slug, columns.join(", "));

    tracing::info!("Creating collection table: {}", slug);
    tracing::debug!("SQL: {}", sql);

    conn.execute(&sql, [])
        .with_context(|| format!("Failed to create table {}", slug))?;

    Ok(())
}

/// Append a DEFAULT value clause to a column definition string.
pub fn append_default_value(col: &mut String, default_value: &Option<serde_json::Value>, field_type: &FieldType) {
    if let Some(ref default) = default_value {
        match default {
            serde_json::Value::String(s) => col.push_str(&format!(" DEFAULT '{}'", s.replace('\'', "''"))),
            serde_json::Value::Number(n) => col.push_str(&format!(" DEFAULT {}", n)),
            serde_json::Value::Bool(b) => col.push_str(&format!(" DEFAULT {}", if *b { 1 } else { 0 })),
            _ => {}
        }
    } else if *field_type == FieldType::Checkbox {
        col.push_str(" DEFAULT 0");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_helpers::*;
    use crate::core::collection::*;
    use crate::core::field::{FieldDefinition, FieldType, FieldTab};
    use crate::db::migrate::helpers::{table_exists, get_table_columns};

    #[test]
    fn create_simple_collection_table() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            text_field("title"),
            text_field("body"),
        ]);
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
        let pool = in_memory_pool();
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
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("users", vec![text_field("email")]);
        def.auth = Some({
            let mut auth = CollectionAuth::new(true);
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
        assert!(cols.contains("_verified"));
        assert!(cols.contains("_verification_token"));
    }

    #[test]
    fn drafts_collection_has_status_column() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.versions = Some(VersionsConfig::new(true, 0));
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("_status"));
    }

    #[test]
    fn group_field_creates_prefixed_columns() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![text_field("meta_title"), text_field("meta_desc")],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__meta_title"));
        assert!(cols.contains("seo__meta_desc"));
        assert!(!cols.contains("seo"), "group field itself should not be a column");
    }

    #[test]
    fn create_with_default_values() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "status".to_string(),
                field_type: FieldType::Text,
                default_value: Some(serde_json::json!("draft")),
                ..Default::default()
            },
            FieldDefinition {
                name: "count".to_string(),
                field_type: FieldType::Number,
                default_value: Some(serde_json::json!(0)),
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        // Just verify it was created (defaults encoded in DDL)
        assert!(table_exists(&conn, "posts").unwrap());
    }

    #[test]
    fn create_with_required_unique_fields() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "slug".to_string(),
                field_type: FieldType::Text,
                required: true,
                unique: true,
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        assert!(table_exists(&conn, "posts").unwrap());
    }

    #[test]
    fn create_collection_no_timestamps() {
        let pool = in_memory_pool();
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
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "title".to_string(),
                        field_type: FieldType::Text,
                        localized: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    #[test]
    fn create_required_localized_field() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "title".to_string(),
                field_type: FieldType::Text,
                localized: true,
                required: true,
                unique: true,
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();

        // Should succeed — NOT NULL only on default locale
        assert!(table_exists(&conn, "posts").unwrap());
    }

    #[test]
    fn create_required_localized_group_subfield() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                localized: true,
                fields: vec![
                    FieldDefinition {
                        name: "title".to_string(),
                        field_type: FieldType::Text,
                        required: true,
                        unique: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    #[test]
    fn row_field_promotes_sub_fields_without_prefix() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "layout".to_string(),
                field_type: FieldType::Row,
                fields: vec![text_field("first_name"), text_field("last_name")],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("first_name"), "Row sub-field should be a top-level column");
        assert!(cols.contains("last_name"), "Row sub-field should be a top-level column");
        assert!(!cols.contains("layout"), "Row field itself should not be a column");
        assert!(!cols.contains("layout__first_name"), "Row should not use prefix");
    }

    #[test]
    fn collapsible_field_promotes_sub_fields_without_prefix() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "details".to_string(),
                field_type: FieldType::Collapsible,
                fields: vec![text_field("summary"), text_field("notes")],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("summary"), "Collapsible sub-field should be promoted");
        assert!(cols.contains("notes"), "Collapsible sub-field should be promoted");
        assert!(!cols.contains("details"), "Collapsible container should not be a column");
        assert!(!cols.contains("details__summary"), "Collapsible should not use prefix");
    }

    #[test]
    fn tabs_field_promotes_sub_fields_without_prefix() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "layout".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![
                    FieldTab::new("Content", vec![text_field("body")]),
                    FieldTab::new("SEO", vec![text_field("meta_title")]),
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("body"), "Tabs sub-field should be promoted");
        assert!(cols.contains("meta_title"), "Tabs sub-field should be promoted");
        assert!(!cols.contains("layout"), "Tabs container should not be a column");
    }

    #[test]
    fn tabs_containing_group_creates_prefixed_columns() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "layout".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![
                    FieldTab::new("Social", vec![
                        FieldDefinition {
                            name: "social".to_string(),
                            field_type: FieldType::Group,
                            fields: vec![text_field("github"), text_field("twitter")],
                            ..Default::default()
                        },
                    ]),
                    FieldTab::new("Content", vec![text_field("body")]),
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("social__github"), "Group inside Tabs should use group__subfield");
        assert!(cols.contains("social__twitter"), "Group inside Tabs should use group__subfield");
        assert!(cols.contains("body"), "Plain field in Tabs should be promoted flat");
        assert!(!cols.contains("social"), "Group itself should not be a column");
    }

    #[test]
    fn collapsible_containing_group_creates_prefixed_columns() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "extra".to_string(),
                field_type: FieldType::Collapsible,
                fields: vec![
                    FieldDefinition {
                        name: "seo".to_string(),
                        field_type: FieldType::Group,
                        fields: vec![text_field("title"), text_field("desc")],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__title"), "Group inside Collapsible should use group__subfield");
        assert!(cols.contains("seo__desc"), "Group inside Collapsible should use group__subfield");
        assert!(!cols.contains("seo"), "Group itself should not be a column");
    }

    #[test]
    fn deeply_nested_tabs_collapsible_group() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "layout".to_string(),
                field_type: FieldType::Tabs,
                tabs: vec![
                    FieldTab::new("Advanced", vec![
                        FieldDefinition {
                            name: "advanced".to_string(),
                            field_type: FieldType::Collapsible,
                            fields: vec![
                                FieldDefinition {
                                    name: "og".to_string(),
                                    field_type: FieldType::Group,
                                    fields: vec![text_field("image"), text_field("title")],
                                    ..Default::default()
                                },
                                text_field("canonical"),
                            ],
                            ..Default::default()
                        },
                    ]),
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("og__image"), "Deeply nested Group inside Collapsible inside Tabs");
        assert!(cols.contains("og__title"), "Deeply nested Group inside Collapsible inside Tabs");
        assert!(cols.contains("canonical"), "Plain field in Collapsible inside Tabs");
    }

    #[test]
    fn group_containing_row() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "row1".to_string(),
                        field_type: FieldType::Row,
                        fields: vec![text_field("title"), text_field("slug")],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("meta__title"), "Group→Row should produce meta__title");
        assert!(cols.contains("meta__slug"), "Group→Row should produce meta__slug");
    }

    #[test]
    fn group_containing_collapsible() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "advanced".to_string(),
                        field_type: FieldType::Collapsible,
                        fields: vec![text_field("robots"), text_field("canonical")],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("seo__robots"), "Group→Collapsible should produce seo__robots");
        assert!(cols.contains("seo__canonical"), "Group→Collapsible should produce seo__canonical");
    }

    #[test]
    fn group_containing_tabs() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "settings".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "layout".to_string(),
                        field_type: FieldType::Tabs,
                        tabs: vec![
                            FieldTab::new("General", vec![text_field("theme")]),
                            FieldTab::new("Advanced", vec![text_field("cache_ttl")]),
                        ],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("settings__theme"), "Group→Tabs should produce settings__theme");
        assert!(cols.contains("settings__cache_ttl"), "Group→Tabs should produce settings__cache_ttl");
    }

    #[test]
    fn group_tabs_group_three_levels() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "outer".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "layout".to_string(),
                        field_type: FieldType::Tabs,
                        tabs: vec![FieldTab::new("Nested", vec![
                            FieldDefinition {
                                name: "inner".to_string(),
                                field_type: FieldType::Group,
                                fields: vec![text_field("deep_value")],
                                ..Default::default()
                            },
                        ])],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("outer__inner__deep_value"), "Group→Tabs→Group should produce outer__inner__deep_value");
    }

    #[test]
    fn group_row_group_collapsible_four_levels() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "a".to_string(),
                field_type: FieldType::Group,
                fields: vec![
                    FieldDefinition {
                        name: "r".to_string(),
                        field_type: FieldType::Row,
                        fields: vec![
                            FieldDefinition {
                                name: "b".to_string(),
                                field_type: FieldType::Group,
                                fields: vec![
                                    FieldDefinition {
                                        name: "c".to_string(),
                                        field_type: FieldType::Collapsible,
                                        fields: vec![text_field("leaf")],
                                        ..Default::default()
                                    },
                                ],
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("a__b__leaf"), "Group→Row→Group→Collapsible: a__b__leaf");
    }

    #[test]
    fn group_containing_tabs_with_locale() {
        let pool = in_memory_pool();
        let conn = pool.get().unwrap();
        let def = simple_collection("posts", vec![
            FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                localized: true,
                fields: vec![
                    FieldDefinition {
                        name: "layout".to_string(),
                        field_type: FieldType::Tabs,
                        tabs: vec![FieldTab::new("Content", vec![text_field("title")])],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        ]);
        create_collection_table(&conn, "posts", &def, &locale_en_de()).unwrap();
        let cols = get_table_columns(&conn, "posts").unwrap();
        assert!(cols.contains("meta__title__en"), "Localized Group→Tabs: meta__title__en");
        assert!(cols.contains("meta__title__de"), "Localized Group→Tabs: meta__title__de");
    }

    #[test]
    fn append_default_string() {
        let mut col = "name TEXT".to_string();
        append_default_value(&mut col, &Some(serde_json::json!("hello")), &FieldType::Text);
        assert!(col.contains("DEFAULT 'hello'"));
    }

    #[test]
    fn append_default_number() {
        let mut col = "count REAL".to_string();
        append_default_value(&mut col, &Some(serde_json::json!(42)), &FieldType::Number);
        assert!(col.contains("DEFAULT 42"));
    }

    #[test]
    fn append_default_bool() {
        let mut col = "active INTEGER".to_string();
        append_default_value(&mut col, &Some(serde_json::json!(true)), &FieldType::Checkbox);
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
}
