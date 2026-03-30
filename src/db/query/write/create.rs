//! Create operation and its helper.

use anyhow::{Context as _, Result, anyhow};
use std::collections::HashMap;

use crate::core::{CollectionDefinition, Document, FieldDefinition, FieldType};
use crate::db::{
    DbConnection, DbValue, LocaleContext,
    query::{
        coerce_value, helpers::normalize_date_with_timezone, locale_write_column,
        read::find_by_id_raw,
    },
};

/// Accumulator for INSERT column/placeholder/param collection during recursive field traversal.
pub(super) struct InsertCollector {
    pub columns: Vec<String>,
    pub placeholders: Vec<String>,
    pub params: Vec<DbValue>,
    pub idx: usize,
}

/// Create a new document. Returns the created document.
pub fn create(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    data: &HashMap<String, String>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let id = nanoid::nanoid!();
    let now = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S.000Z")
        .to_string();

    let mut collector = InsertCollector {
        columns: vec!["id".to_string()],
        placeholders: vec![conn.placeholder(1)],
        params: vec![DbValue::Text(id.clone())],
        idx: 2,
    };

    collect_insert_params(
        &def.fields,
        data,
        &locale_ctx,
        &mut collector,
        conn,
        "",
        false,
    )?;

    if def.timestamps {
        collector.columns.push("created_at".to_string());
        collector.placeholders.push(conn.placeholder(collector.idx));
        collector.params.push(DbValue::Text(now.clone()));
        collector.idx += 1;

        collector.columns.push("updated_at".to_string());
        collector.placeholders.push(conn.placeholder(collector.idx));
        collector.params.push(DbValue::Text(now));
    }

    let sql = format!(
        "INSERT INTO \"{}\" ({}) VALUES ({})",
        slug,
        collector.columns.join(", "),
        collector.placeholders.join(", ")
    );

    conn.execute(&sql, &collector.params)
        .with_context(|| format!("Failed to insert into '{}'", slug))?;

    // Return the created document with the same locale context.
    find_by_id_raw(conn, slug, def, &id, locale_ctx)?
        .ok_or_else(|| anyhow!("Failed to find newly created document"))
}

/// Recursively collect columns, placeholders, and params for INSERT.
/// Handles arbitrary nesting: Group (prefixed), Row/Collapsible/Tabs (promoted flat).
pub(super) fn collect_insert_params(
    fields: &[FieldDefinition],
    data: &HashMap<String, String>,
    locale_ctx: &Option<&LocaleContext>,
    collector: &mut InsertCollector,
    conn: &dyn DbConnection,
    prefix: &str,
    inherited_localized: bool,
) -> Result<()> {
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
                    collector,
                    conn,
                    &new_prefix,
                    inherited_localized || field.localized,
                )?;
            }
            FieldType::Row | FieldType::Collapsible => {
                collect_insert_params(
                    &field.fields,
                    data,
                    locale_ctx,
                    collector,
                    conn,
                    prefix,
                    inherited_localized,
                )?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    collect_insert_params(
                        &tab.fields,
                        data,
                        locale_ctx,
                        collector,
                        conn,
                        prefix,
                        inherited_localized,
                    )?;
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
                let col_name =
                    locale_write_column(&data_key, field, locale_ctx, inherited_localized)?;

                if let Some(value) = data.get(&data_key) {
                    collector.columns.push(col_name);
                    collector.placeholders.push(conn.placeholder(collector.idx));

                    // For Date fields with timezone, use timezone-aware normalization
                    let db_val = if field.field_type == FieldType::Date && field.timezone {
                        let tz_key = format!("{}_tz", data_key);
                        if let Some(tz) = data.get(&tz_key).filter(|s| !s.is_empty()) {
                            if value.is_empty() {
                                DbValue::Null
                            } else {
                                match normalize_date_with_timezone(value, tz) {
                                    Ok(normalized) => DbValue::Text(normalized),
                                    Err(_) => coerce_value(&field.field_type, value),
                                }
                            }
                        } else {
                            coerce_value(&field.field_type, value)
                        }
                    } else {
                        coerce_value(&field.field_type, value)
                    };

                    collector.params.push(db_val);
                    collector.idx += 1;

                    // Timezone companion column for date fields
                    if field.field_type == FieldType::Date && field.timezone {
                        let tz_key = format!("{}_tz", data_key);
                        let tz_col =
                            locale_write_column(&tz_key, field, locale_ctx, inherited_localized)?;
                        collector.columns.push(tz_col);
                        collector.placeholders.push(conn.placeholder(collector.idx));

                        let tz_val = data.get(&tz_key).map(|s| s.as_str()).unwrap_or("");
                        collector.params.push(if tz_val.is_empty() {
                            DbValue::Null
                        } else {
                            DbValue::Text(tz_val.to_string())
                        });
                        collector.idx += 1;
                    }
                } else if field.field_type == FieldType::Checkbox {
                    collector.columns.push(col_name);
                    collector.placeholders.push(conn.placeholder(collector.idx));
                    collector.params.push(DbValue::Integer(0));
                    collector.idx += 1;
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::config::CrapConfig;
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::{BoxedConnection, pool};

    fn setup_db(ddl: &str) -> (TempDir, BoxedConnection) {
        let dir = TempDir::new().unwrap();
        let config = CrapConfig::default();
        let p = pool::create_pool(dir.path(), &config).unwrap();
        let conn = p.get().unwrap();
        conn.execute_batch(ddl).unwrap();
        (dir, conn)
    }

    fn posts_ddl() -> &'static str {
        "CREATE TABLE posts (
            id TEXT PRIMARY KEY,
            title TEXT,
            status TEXT,
            created_at TEXT,
            updated_at TEXT
        )"
    }

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("status", FieldType::Text).build(),
        ];
        def
    }

    #[test]
    fn create_basic() {
        let (_dir, conn) = setup_db(posts_ddl());
        let def = test_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Hello World".to_string());

        let doc = create(&conn, "posts", &def, &data, None).unwrap();
        assert!(!doc.id.is_empty());
        assert_eq!(doc.get_str("title"), Some("Hello World"));
    }

    #[test]
    fn create_with_timestamps() {
        let (_dir, conn) = setup_db(posts_ddl());
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
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                status TEXT,
                published INTEGER,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = test_def();
        def.fields
            .push(FieldDefinition::builder("published", FieldType::Checkbox).build());

        // Create without providing the checkbox field
        let data = HashMap::new();
        let doc = create(&conn, "posts", &def, &data, None).unwrap();

        // Checkbox should default to 0 (integer)
        let published = doc.get("published").unwrap();
        assert_eq!(published, &json!(0));
    }

    #[test]
    fn create_with_group_fields() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                meta__color TEXT,
                meta__size TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

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
        let (_dir, conn) = setup_db(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                name TEXT
            )",
        );

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
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                settings__featured INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            )",
        );

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
        assert_eq!(val, &json!(0));
    }

    #[test]
    fn create_with_collapsible_fields() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                notes TEXT,
                footer TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

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
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                body TEXT,
                slug TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

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
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                social__github TEXT,
                social__twitter TEXT,
                body TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

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
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                og__image TEXT,
                canonical TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

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
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                meta__title TEXT,
                meta__slug TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

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
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                settings__theme TEXT,
                settings__cache_ttl TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

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
        let (_dir, conn) = setup_db(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                outer__inner__deep TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

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

    // ── Timezone companion tests ─────────────────────────────────────

    #[test]
    fn create_date_with_timezone_normalizes_and_stores_tz() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                start_date TEXT,
                start_date_tz TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("events");
        def.fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .timezone(true)
                .build(),
        ];

        let mut data = HashMap::new();
        data.insert("start_date".to_string(), "2024-01-15T09:00".to_string());
        data.insert("start_date_tz".to_string(), "America/New_York".to_string());

        let doc = create(&conn, "events", &def, &data, None).unwrap();

        // 9am EST = 2pm UTC
        assert_eq!(doc.get_str("start_date"), Some("2024-01-15T14:00:00.000Z"));
        assert_eq!(doc.get_str("start_date_tz"), Some("America/New_York"));
    }

    #[test]
    fn create_date_with_timezone_flag_but_no_tz_value_falls_back() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                start_date TEXT,
                start_date_tz TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("events");
        def.fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .timezone(true)
                .build(),
        ];

        let mut data = HashMap::new();
        data.insert("start_date".to_string(), "2024-01-15T09:00".to_string());
        // No timezone value provided

        let doc = create(&conn, "events", &def, &data, None).unwrap();

        // Falls back to normal normalization (treat as UTC)
        assert_eq!(doc.get_str("start_date"), Some("2024-01-15T09:00:00.000Z"));
    }

    #[test]
    fn create_date_without_timezone_flag_no_tz_column() {
        let (_dir, conn) = setup_db(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                event_date TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("events");
        def.fields = vec![FieldDefinition::builder("event_date", FieldType::Date).build()];

        let mut data = HashMap::new();
        data.insert("event_date".to_string(), "2024-01-15".to_string());

        let doc = create(&conn, "events", &def, &data, None).unwrap();
        assert_eq!(doc.get_str("event_date"), Some("2024-01-15T12:00:00.000Z"));
    }

    #[test]
    fn create_read_roundtrip_with_timezone() {
        // Full create/read roundtrip: create a document with a timezone-aware
        // date field, then read it back and verify both the date and _tz
        // companion column are present.
        let (_dir, conn) = setup_db(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                title TEXT,
                start_date TEXT,
                start_date_tz TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("events");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            FieldDefinition::builder("start_date", FieldType::Date)
                .timezone(true)
                .build(),
        ];

        let mut data = HashMap::new();
        data.insert("title".to_string(), "Conference".to_string());
        data.insert("start_date".to_string(), "2024-06-15T09:00".to_string());
        data.insert("start_date_tz".to_string(), "America/New_York".to_string());

        let doc = create(&conn, "events", &def, &data, None).unwrap();

        // Verify the document has both the normalized date and timezone
        assert_eq!(doc.get_str("title"), Some("Conference"));
        assert_eq!(
            doc.get_str("start_date"),
            Some("2024-06-15T13:00:00.000Z"),
            "9am EDT (summer) should be normalized to 1pm UTC"
        );
        assert_eq!(
            doc.get_str("start_date_tz"),
            Some("America/New_York"),
            "Timezone companion column should be stored"
        );
    }

    #[test]
    fn create_read_roundtrip_timezone_in_group() {
        // Timezone-aware date field inside a Group: both the prefixed date
        // and prefixed _tz companion column should survive a create/read roundtrip.
        let (_dir, conn) = setup_db(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                schedule__start TEXT,
                schedule__start_tz TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("events");
        def.fields = vec![
            FieldDefinition::builder("schedule", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("start", FieldType::Date)
                        .timezone(true)
                        .build(),
                ])
                .build(),
        ];

        let mut data = HashMap::new();
        data.insert(
            "schedule__start".to_string(),
            "2024-06-15T09:00".to_string(),
        );
        data.insert(
            "schedule__start_tz".to_string(),
            "Europe/Berlin".to_string(),
        );

        let doc = create(&conn, "events", &def, &data, None).unwrap();

        // Berlin in June is CEST (UTC+2), so 09:00 local = 07:00 UTC
        assert_eq!(
            doc.get_str("schedule__start"),
            Some("2024-06-15T07:00:00.000Z"),
            "Group date should be normalized with timezone"
        );
        assert_eq!(
            doc.get_str("schedule__start_tz"),
            Some("Europe/Berlin"),
            "Group _tz companion should be stored"
        );
    }

    #[test]
    fn create_date_empty_value_with_timezone_stores_null() {
        // When the date value is empty but a timezone is provided,
        // the date should be stored as NULL, not normalized.
        let (_dir, conn) = setup_db(
            "CREATE TABLE events (
                id TEXT PRIMARY KEY,
                start_date TEXT,
                start_date_tz TEXT,
                created_at TEXT,
                updated_at TEXT
            )",
        );

        let mut def = CollectionDefinition::new("events");
        def.fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .timezone(true)
                .build(),
        ];

        let mut data = HashMap::new();
        data.insert("start_date".to_string(), String::new());
        data.insert("start_date_tz".to_string(), "America/New_York".to_string());

        let doc = create(&conn, "events", &def, &data, None).unwrap();

        // Empty date with timezone should result in null
        assert!(
            doc.get("start_date").is_none_or(|v| v.is_null()),
            "Empty date value should be stored as null"
        );
    }
}
