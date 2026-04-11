//! Collection table creation from Lua definitions.

use anyhow::{Context as _, Result};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, FieldType},
    db::{DbConnection, migrate::helpers::collect_column_specs, query::helpers::locale_column},
};

/// Column constraint options for `build_column_def`.
struct ColumnConstraints<'a> {
    required: bool,
    unique: bool,
    soft_delete: bool,
    default_value: &'a Option<Value>,
    field_type: &'a FieldType,
    db_kind: &'a str,
}

/// Build a column definition string with type, constraints, and default.
fn build_column_def(col_name: &str, col_type: &str, constraints: &ColumnConstraints) -> String {
    let mut col = format!("{} {}", col_name, col_type);

    if constraints.required {
        col.push_str(" NOT NULL");
    }

    // Skip inline UNIQUE for soft-delete collections — a partial
    // unique index (WHERE _deleted_at IS NULL) is created instead
    // by sync_indexes so that deleted rows don't block new inserts.
    if constraints.unique && !constraints.soft_delete {
        col.push_str(" UNIQUE");
    }

    append_default_value_for(
        &mut col,
        constraints.default_value,
        constraints.field_type,
        constraints.db_kind,
    );

    col
}

pub fn create_collection_table(
    conn: &dyn DbConnection,
    slug: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];

    collect_field_columns(&mut columns, conn, def, locale_config)?;
    collect_system_columns(&mut columns, conn, def);

    let sql = format!("CREATE TABLE \"{}\" ({})", slug, columns.join(", "));

    info!("Creating collection table: {}", slug);
    debug!("SQL: {}", sql);

    conn.execute_ddl(&sql, &[])
        .with_context(|| format!("Failed to create table {}", slug))?;

    Ok(())
}

/// Collect user-defined field columns (including localized variants).
fn collect_field_columns(
    columns: &mut Vec<String>,
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
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
                let is_required = !spec.companion_text
                    && spec.field.required
                    && *locale == locale_config.default_locale
                    && !def.has_drafts();

                if spec.companion_text {
                    columns.push(format!("{} TEXT", col_name));
                } else {
                    let c = ColumnConstraints {
                        required: is_required,
                        unique: spec.field.unique,
                        soft_delete: def.soft_delete,
                        default_value: &spec.field.default_value,
                        field_type: &spec.field.field_type,
                        db_kind: conn.kind(),
                    };
                    columns.push(build_column_def(&col_name, col_type, &c));
                }
            }
        } else if spec.companion_text {
            columns.push(format!("{} TEXT", spec.col_name));
        } else {
            let c = ColumnConstraints {
                required: spec.field.required && !def.has_drafts(),
                unique: spec.field.unique,
                soft_delete: def.soft_delete,
                default_value: &spec.field.default_value,
                field_type: &spec.field.field_type,
                db_kind: conn.kind(),
            };
            columns.push(build_column_def(&spec.col_name, col_type, &c));
        }
    }

    Ok(())
}

/// Collect system columns (status, auth, timestamps, etc.).
fn collect_system_columns(
    columns: &mut Vec<String>,
    conn: &dyn DbConnection,
    def: &CollectionDefinition,
) {
    if def.has_drafts() {
        columns.push("_status TEXT NOT NULL DEFAULT 'published'".to_string());
    }

    if def.soft_delete {
        columns.push(format!("_deleted_at {}", conn.timestamp_column_type()));
    }

    columns.push("_ref_count INTEGER NOT NULL DEFAULT 0".to_string());

    if def.is_auth_collection() {
        columns.extend([
            "_password_hash TEXT".to_string(),
            "_reset_token TEXT".to_string(),
            "_reset_token_exp INTEGER".to_string(),
            "_locked INTEGER DEFAULT 0".to_string(),
            "_settings TEXT".to_string(),
            "_session_version INTEGER DEFAULT 0".to_string(),
        ]);

        if def.auth.as_ref().is_some_and(|a| a.verify_email) {
            columns.extend([
                "_verified INTEGER DEFAULT 0".to_string(),
                "_verification_token TEXT".to_string(),
                "_verification_token_exp INTEGER".to_string(),
            ]);
        }
    }

    if def.timestamps {
        columns.push(format!("created_at {}", conn.timestamp_column_default()));
        columns.push(format!("updated_at {}", conn.timestamp_column_default()));
    }
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
            warn!(
                "String default value on {:?} field — possible type mismatch",
                field_type
            );
        }
        (Value::Bool(_), FieldType::Text | FieldType::Textarea | FieldType::Email) => {
            warn!(
                "Bool default value on {:?} field — possible type mismatch",
                field_type
            );
        }
        (Value::Number(_), FieldType::Checkbox) => {
            warn!("Number default value on Checkbox field — use a bool default instead");
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
    use crate::db::migrate::helpers::get_table_columns;

    /// Create a collection table and return its column names.
    fn create_and_columns(
        slug: &str,
        def: &CollectionDefinition,
        locale: &LocaleConfig,
    ) -> std::collections::HashSet<String> {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        create_collection_table(&conn, slug, def, locale).unwrap();

        get_table_columns(&conn, slug).unwrap()
    }

    #[test]
    fn create_simple_collection_table() {
        let def = simple_collection("posts", vec![text_field("title"), text_field("body")]);
        let cols = create_and_columns("posts", &def, &no_locale());

        assert!(cols.contains("id"));
        assert!(cols.contains("title"));
        assert!(cols.contains("body"));
        assert!(cols.contains("created_at"));
        assert!(cols.contains("updated_at"));
    }

    #[test]
    fn create_with_localized_fields() {
        let def = simple_collection("posts", vec![localized_field("title")]);
        let cols = create_and_columns("posts", &def, &locale_en_de());

        assert!(cols.contains("title__en"), "should have en locale column");
        assert!(cols.contains("title__de"), "should have de locale column");
        assert!(!cols.contains("title"), "should NOT have bare title column");
    }

    #[test]
    fn create_auth_collection_has_system_columns() {
        let mut def = simple_collection("users", vec![text_field("email")]);
        def.auth = Some({
            let mut auth = Auth::new(true);
            auth.verify_email = true;
            auth
        });
        let cols = create_and_columns("users", &def, &no_locale());
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
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.versions = Some(VersionsConfig::new(true, 0));
        let cols = create_and_columns("posts", &def, &no_locale());

        assert!(cols.contains("_status"));
    }

    #[test]
    fn group_field_creates_prefixed_columns() {
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("seo", FieldType::Group)
                    .fields(vec![text_field("meta_title"), text_field("meta_desc")])
                    .build(),
            ],
        );
        let cols = create_and_columns("posts", &def, &no_locale());

        assert!(cols.contains("seo__meta_title"));
        assert!(cols.contains("seo__meta_desc"));
        assert!(
            !cols.contains("seo"),
            "group field itself should not be a column"
        );
    }

    #[test]
    fn create_with_default_values() {
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
        // Just verify it was created (defaults encoded in DDL)
        let _ = create_and_columns("posts", &def, &no_locale());
    }

    #[test]
    fn create_with_required_unique_fields() {
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("slug", FieldType::Text)
                    .required(true)
                    .unique(true)
                    .build(),
            ],
        );
        let _ = create_and_columns("posts", &def, &no_locale());
    }

    #[test]
    fn create_collection_no_timestamps() {
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.timestamps = false;
        let cols = create_and_columns("posts", &def, &no_locale());

        assert!(!cols.contains("created_at"));
        assert!(!cols.contains("updated_at"));
    }

    #[test]
    fn create_localized_group_subfield() {
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
        let cols = create_and_columns("posts", &def, &locale_en_de());

        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    #[test]
    fn create_required_localized_field() {
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
        let _ = create_and_columns("posts", &def, &locale_en_de());
    }

    #[test]
    fn create_required_localized_group_subfield() {
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
        let cols = create_and_columns("posts", &def, &locale_en_de());
        assert!(cols.contains("seo__title__en"));
        assert!(cols.contains("seo__title__de"));
    }

    #[test]
    fn row_field_promotes_sub_fields_without_prefix() {
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("layout", FieldType::Row)
                    .fields(vec![text_field("first_name"), text_field("last_name")])
                    .build(),
            ],
        );
        let cols = create_and_columns("posts", &def, &no_locale());
        assert!(cols.contains("first_name"));
        assert!(cols.contains("last_name"));
        assert!(!cols.contains("layout"));
        assert!(!cols.contains("layout__first_name"));
    }

    #[test]
    fn collapsible_field_promotes_sub_fields_without_prefix() {
        let def = simple_collection(
            "posts",
            vec![
                FieldDefinition::builder("details", FieldType::Collapsible)
                    .fields(vec![text_field("summary"), text_field("notes")])
                    .build(),
            ],
        );
        let cols = create_and_columns("posts", &def, &no_locale());
        assert!(cols.contains("summary"));
        assert!(cols.contains("notes"));
        assert!(!cols.contains("details"));
        assert!(!cols.contains("details__summary"));
    }

    #[test]
    fn tabs_field_promotes_sub_fields_without_prefix() {
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
        let cols = create_and_columns("posts", &def, &no_locale());
        assert!(cols.contains("body"));
        assert!(cols.contains("meta_title"));
        assert!(!cols.contains("layout"));
    }

    #[test]
    fn tabs_containing_group_creates_prefixed_columns() {
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
        let cols = create_and_columns("posts", &def, &no_locale());
        assert!(cols.contains("social__github"));
        assert!(cols.contains("social__twitter"));
        assert!(cols.contains("body"));
        assert!(!cols.contains("social"));
    }

    #[test]
    fn collapsible_containing_group_creates_prefixed_columns() {
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
        let cols = create_and_columns("posts", &def, &no_locale());
        assert!(cols.contains("seo__title"));
        assert!(cols.contains("seo__desc"));
        assert!(!cols.contains("seo"));
    }

    #[test]
    fn deeply_nested_tabs_collapsible_group() {
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
        let cols = create_and_columns("posts", &def, &no_locale());
        assert!(cols.contains("og__image"));
        assert!(cols.contains("og__title"));
        assert!(cols.contains("canonical"));
    }

    #[test]
    fn group_containing_row() {
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
        let cols = create_and_columns("posts", &def, &no_locale());
        assert!(cols.contains("meta__title"));
        assert!(cols.contains("meta__slug"));
    }

    #[test]
    fn group_containing_collapsible() {
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
        let cols = create_and_columns("posts", &def, &no_locale());
        assert!(cols.contains("seo__robots"));
        assert!(cols.contains("seo__canonical"));
    }

    #[test]
    fn group_containing_tabs() {
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
        let cols = create_and_columns("posts", &def, &no_locale());
        assert!(cols.contains("settings__theme"));
        assert!(cols.contains("settings__cache_ttl"));
    }

    #[test]
    fn group_tabs_group_three_levels() {
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
        let cols = create_and_columns("posts", &def, &no_locale());
        assert!(cols.contains("outer__inner__deep_value"));
    }

    #[test]
    fn group_row_group_collapsible_four_levels() {
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
        let cols = create_and_columns("posts", &def, &no_locale());
        assert!(cols.contains("a__b__leaf"));
    }

    #[test]
    fn group_containing_tabs_with_locale() {
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
        let cols = create_and_columns("posts", &def, &locale_en_de());
        assert!(cols.contains("meta__title__en"));
        assert!(cols.contains("meta__title__de"));
    }

    // ── Default value tests ─────────────────────────────────────────────

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
        let mut def = simple_collection("posts", vec![text_field("title")]);
        def.soft_delete = true;
        let cols = create_and_columns("posts", &def, &no_locale());
        assert!(cols.contains("_deleted_at"));
    }

    #[test]
    fn non_soft_delete_collection_has_no_deleted_at_column() {
        let def = simple_collection("posts", vec![text_field("title")]);
        let cols = create_and_columns("posts", &def, &no_locale());
        assert!(!cols.contains("_deleted_at"));
    }

    #[test]
    fn create_date_field_with_timezone_creates_tz_column() {
        let def = simple_collection(
            "events",
            vec![
                FieldDefinition::builder("starts_at", FieldType::Date)
                    .timezone(true)
                    .build(),
            ],
        );
        let cols = create_and_columns("events", &def, &no_locale());
        assert!(cols.contains("starts_at"));
        assert!(cols.contains("starts_at_tz"));
    }

    #[test]
    fn create_date_field_without_timezone_has_no_tz_column() {
        let def = simple_collection(
            "events",
            vec![FieldDefinition::builder("starts_at", FieldType::Date).build()],
        );
        let cols = create_and_columns("events", &def, &no_locale());
        assert!(cols.contains("starts_at"));
        assert!(!cols.contains("starts_at_tz"));
    }

    #[test]
    fn create_localized_date_with_timezone() {
        let def = simple_collection(
            "events",
            vec![
                FieldDefinition::builder("starts_at", FieldType::Date)
                    .timezone(true)
                    .localized(true)
                    .build(),
            ],
        );
        let cols = create_and_columns("events", &def, &locale_en_de());
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
