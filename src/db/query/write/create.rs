//! Create operation and its helper.

use anyhow::{Context as _, Result};
use rusqlite::params_from_iter;
use std::collections::HashMap;

use super::super::read::find_by_id_raw;
use super::super::{LocaleContext, coerce_value, locale_write_column};
use crate::core::field::FieldType;
use crate::core::{CollectionDefinition, Document};

/// Create a new document. Returns the created document.
pub fn create(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    data: &HashMap<String, String>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let id = nanoid::nanoid!();
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S.000Z")
        .to_string();

    let mut columns = vec!["id".to_string()];
    let mut placeholders = vec!["?1".to_string()];
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(id.clone())];
    let mut idx = 2;

    collect_insert_params(
        &def.fields,
        data,
        &locale_ctx,
        &mut columns,
        &mut placeholders,
        &mut params,
        &mut idx,
        "",
    );

    if def.timestamps {
        columns.push("created_at".to_string());
        placeholders.push(format!("?{}", idx));
        params.push(Box::new(now.clone()));
        idx += 1;

        columns.push("updated_at".to_string());
        placeholders.push(format!("?{}", idx));
        params.push(Box::new(now));
    }

    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        slug,
        columns.join(", "),
        placeholders.join(", ")
    );

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();

    conn.execute(&sql, params_from_iter(param_refs.iter()))
        .with_context(|| format!("Failed to insert into '{}'", slug))?;

    // Return the created document with the same locale context
    find_by_id_raw(conn, slug, def, &id, locale_ctx)?
        .ok_or_else(|| anyhow::anyhow!("Failed to find newly created document"))
}

/// Recursively collect columns, placeholders, and params for INSERT.
/// Handles arbitrary nesting: Group (prefixed), Row/Collapsible/Tabs (promoted flat).
pub(super) fn collect_insert_params(
    fields: &[crate::core::field::FieldDefinition],
    data: &HashMap<String, String>,
    locale_ctx: &Option<&LocaleContext>,
    columns: &mut Vec<String>,
    placeholders: &mut Vec<String>,
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
                collect_insert_params(
                    &field.fields,
                    data,
                    locale_ctx,
                    columns,
                    placeholders,
                    params,
                    idx,
                    &new_prefix,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_insert_params(
                    &field.fields,
                    data,
                    locale_ctx,
                    columns,
                    placeholders,
                    params,
                    idx,
                    prefix,
                );
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_insert_params(
                        &tab.fields,
                        data,
                        locale_ctx,
                        columns,
                        placeholders,
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
                    columns.push(col_name);
                    placeholders.push(format!("?{}", *idx));
                    params.push(coerce_value(&field.field_type, value));
                    *idx += 1;
                } else if field.field_type == FieldType::Checkbox {
                    columns.push(col_name);
                    placeholders.push(format!("?{}", *idx));
                    params.push(Box::new(0i32));
                    *idx += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;
    use crate::core::field::*;
    use rusqlite::Connection;

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Text).build(),
        ];
        def
    }

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                status TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();
        conn
    }

    #[test]
    fn create_basic() {
        let conn = setup_db();
        let def = test_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Hello World".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert!(!doc.id.is_empty());
        assert_eq!(doc.get_str("title"), Some("Hello World"));
    }

    #[test]
    fn create_with_timestamps() {
        let conn = setup_db();
        let def = test_def();
        let data = HashMap::new();

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert!(doc.created_at.is_some(), "created_at should be set");
        assert!(doc.updated_at.is_some(), "updated_at should be set");
        // Both should be the same on creation
        assert_eq!(doc.created_at, doc.updated_at);
    }

    #[test]
    fn create_checkbox_defaults_to_zero() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                status TEXT,
                published INTEGER,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = test_def();
        def.fields
            .push(FieldDefinition::builder("published", FieldType::Checkbox).build());

        // Create without providing the checkbox field
        let data = HashMap::new();
        let doc = create(&conn, "posts", &def, &data, None).unwrap();

        // Checkbox should default to 0 (integer)
        let published = doc.get("published").unwrap();
        assert_eq!(published, &serde_json::json!(0));
    }

    #[test]
    fn create_with_group_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                meta__color TEXT,
                meta__size TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("color", FieldType::Text).build(),
                    FieldDefinition::builder("size", FieldType::Text).build(),
                ])
                .build(),
        ];
        let def = def;

        let mut data = HashMap::new();
        data.insert("title".to_string(), "Post1".to_string());
        data.insert("meta__color".to_string(), "red".to_string());
        data.insert("meta__size".to_string(), "large".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("title"), Some("Post1"));
        // Group sub-fields stored as prefixed columns
        assert_eq!(doc.get_str("meta__color"), Some("red"));
        assert_eq!(doc.get_str("meta__size"), Some("large"));
    }

    #[test]
    fn create_without_timestamps() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                name TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("events");
        def.timestamps = false;
        def.fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];
        let def = def;

        let mut data = HashMap::new();
        data.insert("name".to_string(), "Event1".to_string());

        let doc = create(&conn, "events", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("name"), Some("Event1"));
        assert!(
            doc.created_at.is_none(),
            "no timestamps collection should have no created_at"
        );
        assert!(
            doc.updated_at.is_none(),
            "no timestamps collection should have no updated_at"
        );
    }

    #[test]
    fn create_group_with_checkbox_sub_field_default() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                settings__featured INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("settings", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("featured", FieldType::Checkbox).build(),
                ])
                .build(),
        ];
        let def = def;

        // Create without providing the checkbox group sub-field — should default to 0
        let data = HashMap::new();
        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        let val = doc.get("settings__featured").unwrap();
        assert_eq!(val, &serde_json::json!(0));
    }

    #[test]
    fn create_with_collapsible_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                notes TEXT,
                footer TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("extra", FieldType::Collapsible)
                .fields(vec![
                    FieldDefinition::builder("notes", FieldType::Text).build(),
                    FieldDefinition::builder("footer", FieldType::Text).build(),
                ])
                .build(),
        ];
        let def = def;

        let mut data = HashMap::new();
        data.insert("notes".to_string(), "Some notes".to_string());
        data.insert("footer".to_string(), "Copyright".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("notes"), Some("Some notes"));
        assert_eq!(doc.get_str("footer"), Some("Copyright"));
    }

    #[test]
    fn create_with_tabs_fields() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                body TEXT,
                slug TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![
                    FieldTab::new(
                        "Content",
                        vec![FieldDefinition::builder("body", FieldType::Text).build()],
                    ),
                    FieldTab::new(
                        "Meta",
                        vec![FieldDefinition::builder("slug", FieldType::Text).build()],
                    ),
                ])
                .build(),
        ];
        let def = def;

        let mut data = HashMap::new();
        data.insert("body".to_string(), "Hello world".to_string());
        data.insert("slug".to_string(), "hello-world".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("body"), Some("Hello world"));
        assert_eq!(doc.get_str("slug"), Some("hello-world"));
    }

    #[test]
    fn create_with_tabs_containing_group() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                social__github TEXT,
                social__twitter TEXT,
                body TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![
                    FieldTab::new(
                        "Social",
                        vec![
                            FieldDefinition::builder("social", FieldType::Group)
                                .fields(vec![
                                    FieldDefinition::builder("github", FieldType::Text).build(),
                                    FieldDefinition::builder("twitter", FieldType::Text).build(),
                                ])
                                .build(),
                        ],
                    ),
                    FieldTab::new(
                        "Content",
                        vec![FieldDefinition::builder("body", FieldType::Text).build()],
                    ),
                ])
                .build(),
        ];
        let def = def;

        let mut data = HashMap::new();
        data.insert(
            "social__github".to_string(),
            "https://github.com".to_string(),
        );
        data.insert("social__twitter".to_string(), "@test".to_string());
        data.insert("body".to_string(), "Content here".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("social__github"), Some("https://github.com"));
        assert_eq!(doc.get_str("social__twitter"), Some("@test"));
        assert_eq!(doc.get_str("body"), Some("Content here"));
    }

    #[test]
    fn create_deeply_nested_tabs_collapsible_group() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                og__image TEXT,
                canonical TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("layout", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Advanced",
                    vec![
                        FieldDefinition::builder("advanced", FieldType::Collapsible)
                            .fields(vec![
                                FieldDefinition::builder("og", FieldType::Group)
                                    .fields(vec![
                                        FieldDefinition::builder("image", FieldType::Text).build(),
                                    ])
                                    .build(),
                                FieldDefinition::builder("canonical", FieldType::Text).build(),
                            ])
                            .build(),
                    ],
                )])
                .build(),
        ];
        let def = def;

        let mut data = HashMap::new();
        data.insert("og__image".to_string(), "hero.jpg".to_string());
        data.insert("canonical".to_string(), "https://example.com".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("og__image"), Some("hero.jpg"));
        assert_eq!(doc.get_str("canonical"), Some("https://example.com"));
    }

    #[test]
    fn create_group_containing_row() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                meta__title TEXT,
                meta__slug TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("r", FieldType::Row)
                        .fields(vec![
                            FieldDefinition::builder("title", FieldType::Text).build(),
                            FieldDefinition::builder("slug", FieldType::Text).build(),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let def = def;

        let mut data = HashMap::new();
        data.insert("meta__title".to_string(), "Hello".to_string());
        data.insert("meta__slug".to_string(), "hello".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("meta__title"), Some("Hello"));
        assert_eq!(doc.get_str("meta__slug"), Some("hello"));
    }

    #[test]
    fn create_group_containing_tabs() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                settings__theme TEXT,
                settings__cache_ttl TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("settings", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("layout", FieldType::Tabs)
                        .tabs(vec![
                            FieldTab::new(
                                "General",
                                vec![FieldDefinition::builder("theme", FieldType::Text).build()],
                            ),
                            FieldTab::new(
                                "Advanced",
                                vec![
                                    FieldDefinition::builder("cache_ttl", FieldType::Text).build(),
                                ],
                            ),
                        ])
                        .build(),
                ])
                .build(),
        ];
        let def = def;

        let mut data = HashMap::new();
        data.insert("settings__theme".to_string(), "dark".to_string());
        data.insert("settings__cache_ttl".to_string(), "3600".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("settings__theme"), Some("dark"));
        assert_eq!(doc.get_str("settings__cache_ttl"), Some("3600"));
    }

    #[test]
    fn create_group_tabs_group_three_levels() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                outer__inner__deep TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("outer", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("t", FieldType::Tabs)
                        .tabs(vec![FieldTab::new(
                            "Tab",
                            vec![
                                FieldDefinition::builder("inner", FieldType::Group)
                                    .fields(vec![
                                        FieldDefinition::builder("deep", FieldType::Text).build(),
                                    ])
                                    .build(),
                            ],
                        )])
                        .build(),
                ])
                .build(),
        ];
        let def = def;

        let mut data = HashMap::new();
        data.insert("outer__inner__deep".to_string(), "bottom".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("outer__inner__deep"), Some("bottom"));
    }
}
