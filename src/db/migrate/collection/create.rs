//! Collection table creation from Lua definitions.

use std::fmt::Write as _;

use anyhow::{Context as _, Result};
use serde_json::Value;
use tracing::{debug, info, warn};

use super::system_columns::{
    AUTH_COLUMNS, DRAFT_STATUS_COLUMN, MFA_COLUMNS, REF_COUNT_COLUMN, TOTP_COLUMNS,
    VERIFY_EMAIL_COLUMNS,
};
use crate::{
    config::LocaleConfig,
    core::{
        CollectionDefinition, FieldDefinition, FieldType,
        collection::{Auth, MfaMode},
    },
    db::{
        DbConnection, DbValue,
        migrate::helpers::collect_column_specs,
        query::helpers::{column_value, locale_column, quote_ident},
        types::real_to_json_number,
    },
};

/// Build a column definition string with type, constraints, and default.
///
/// A `unique` field gets no inline `UNIQUE` here: uniqueness is a managed
/// index that `sync_indexes` owns, so a field that gains `unique` later is
/// enforced exactly like one that had it at creation.
fn build_column_def(
    col_name: &str,
    col_type: &str,
    required: bool,
    field: &FieldDefinition,
) -> String {
    let mut col = format!("{} {col_type}", quote_ident(col_name));

    if required {
        col.push_str(" NOT NULL");
    }

    append_default_value_for(&mut col, field);

    col
}

/// Create the table a collection's documents live in.
///
/// `table` is the name to create it under. That is the collection's slug
/// everywhere but the soft-delete rebuild, which assembles the replacement
/// under a temporary name and renames it into place afterwards. No indexes are
/// created here — `sync_indexes` owns those, and a rebuild depends on the
/// temporary table carrying none that would collide with the managed names.
pub(crate) fn create_collection_table(
    conn: &dyn DbConnection,
    table: &str,
    def: &CollectionDefinition,
    locale_config: &LocaleConfig,
) -> Result<()> {
    let mut columns = vec!["id TEXT PRIMARY KEY".to_string()];

    collect_field_columns(&mut columns, conn, def, locale_config)?;
    collect_system_columns(&mut columns, conn, def);

    let sql = format!("CREATE TABLE \"{}\" ({})", table, columns.join(", "));

    info!("Creating collection table: {}", table);
    debug!("SQL: {}", sql);

    conn.execute_ddl(&sql, &[])
        .with_context(|| format!("Failed to create table {table}"))?;

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
        let col_type = spec.ddl_type(conn);

        if spec.is_localized {
            for locale in &locale_config.locales {
                let col_name = locale_column(&spec.col_name, locale)?;
                let is_required = !spec.companion_text
                    && spec.field.required
                    && *locale == locale_config.default_locale
                    && !def.has_drafts();

                if spec.companion_text {
                    columns.push(format!("{} TEXT", quote_ident(&col_name)));
                } else {
                    columns.push(build_column_def(
                        &col_name,
                        col_type,
                        is_required,
                        spec.field,
                    ));
                }
            }
        } else if spec.companion_text {
            columns.push(format!("{} TEXT", quote_ident(&spec.col_name)));
        } else {
            let required = spec.field.required && !def.has_drafts();
            columns.push(build_column_def(
                &spec.col_name,
                col_type,
                required,
                spec.field,
            ));
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
        columns.push(DRAFT_STATUS_COLUMN.to_string());
    }

    if def.soft_delete {
        columns.push(format!("_deleted_at {}", conn.timestamp_column_type()));
    }

    columns.push(REF_COUNT_COLUMN.to_string());

    if def.is_auth_collection() {
        columns.extend(AUTH_COLUMNS.iter().map(|c| (*c).to_string()));

        if def.auth.as_ref().is_some_and(Auth::requires_verify_email) {
            columns.extend(VERIFY_EMAIL_COLUMNS.iter().map(|c| (*c).to_string()));
        }

        // MFA columns (parallel to alter::add_auth_columns via the shared
        // `system_columns` consts). Without these, `set_mfa_code` fails silently
        // on a freshly-created auth collection with `mfa = Email`, so the MFA
        // challenge email never gets queued.
        if def.auth.as_ref().is_some_and(|a| a.mfa() != MfaMode::Off) {
            columns.extend(MFA_COLUMNS.iter().map(|c| (*c).to_string()));
        }

        if def.auth.as_ref().is_some_and(|a| a.mfa() == MfaMode::Totp) {
            columns.extend(TOTP_COLUMNS.iter().map(|c| (*c).to_string()));
        }
    }

    if def.timestamps {
        columns.push(format!("created_at {}", conn.timestamp_column_default()));
        columns.push(format!("updated_at {}", conn.timestamp_column_default()));
    }
}

/// Append the DEFAULT clause of `field`'s column: its default value, or `0` for
/// a checkbox without one. Checkbox values are `0`/`1` (INTEGER on all
/// backends).
pub(crate) fn append_default_value_for(col: &mut String, field: &FieldDefinition) {
    let Some(default) = field.default_value.as_ref() else {
        if field.field_type == FieldType::Checkbox {
            col.push_str(" DEFAULT 0");
        }
        return;
    };

    warn_default_type_mismatch(default, &field.field_type);

    // The literal is the value a write of the default stores — a date
    // normalized, text canonical, a has-many list as its canonical JSON — so a
    // row created without the field holds the same form as one written with it.
    match column_value(field, default, None) {
        DbValue::Text(s) => {
            let _ = write!(col, " DEFAULT '{}'", s.replace('\'', "''"));
        }
        DbValue::Integer(i) => {
            let _ = write!(col, " DEFAULT {i}");
        }
        DbValue::Real(r) => {
            let _ = write!(col, " DEFAULT {}", real_to_json_number(r));
        }
        DbValue::Null | DbValue::Blob(_) => {}
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
    use std::collections::HashSet;

    use serde_json::json;

    use super::*;
    use crate::{
        core::{FieldTab, collection::*},
        db::{
            migrate::{
                collection::{sync_collection_table, test_helpers::*},
                helpers::{get_table_column_types, get_table_columns},
            },
            query::helpers::coerce_value,
        },
    };

    /// The DEFAULT clause `field_type` with `default` appends to `col`.
    fn with_default(col: &str, field_type: FieldType, default: Option<Value>) -> String {
        let mut builder = FieldDefinition::builder("f", field_type);
        if let Some(default) = default {
            builder = builder.default_value(default);
        }

        let mut col = col.to_string();
        append_default_value_for(&mut col, &builder.build());

        col
    }

    /// Create a collection table and return its column names.
    fn create_and_columns(
        slug: &str,
        def: &CollectionDefinition,
        locale: &LocaleConfig,
    ) -> HashSet<String> {
        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();

        create_collection_table(&conn, slug, def, locale).unwrap();

        get_table_columns(&conn, slug).unwrap()
    }

    /// Regression: a date default became the column DEFAULT as written, while a
    /// write stores dates normalized — a row created without the field held a
    /// different form than one written with the same value.
    #[test]
    fn a_date_default_is_stored_normalized() {
        let col = with_default(
            "\"starts\" TEXT",
            FieldType::Date,
            Some(json!("2026-01-01")),
        );

        let DbValue::Text(stored) = coerce_value(&FieldType::Date, "2026-01-01") else {
            panic!("a date encodes as text");
        };
        assert_eq!(col, format!("\"starts\" TEXT DEFAULT '{stored}'"));
    }

    /// Regression: a has-many default became the column DEFAULT through the
    /// single-value coercion — a number list stored nothing, a text list its
    /// JSON as sent — where a write of the same default stores the canonical
    /// list.
    #[test]
    fn a_has_many_default_is_stored_as_its_write_stores_it() {
        for (field_type, default) in [
            (FieldType::Number, json!([1, 2])),
            (FieldType::Text, json!([1, "b"])),
        ] {
            let def = simple_collection(
                "posts",
                vec![
                    FieldDefinition::builder("items", field_type)
                        .has_many(true)
                        .default_value(default.clone())
                        .build(),
                ],
            );
            let (_dir, pool) = in_memory_pool();
            let conn = pool.get().unwrap();
            create_collection_table(&conn, "posts", &def, &no_locale()).unwrap();

            conn.execute("INSERT INTO posts (id) VALUES ('a')", &[])
                .unwrap();
            let row = conn
                .query_one("SELECT items FROM posts WHERE id = 'a'", &[])
                .unwrap()
                .unwrap();

            assert_eq!(
                row.opt_text_at(0).map_or(DbValue::Null, DbValue::Text),
                column_value(&def.fields[0], &default, None),
                "{default}"
            );
        }
    }

    #[test]
    fn integer_flag_keeps_number_column_real() {
        // `integer = true` is a validation/UI constraint only — it must not
        // change the column type, so no migration is implied. A number field
        // stays REAL whether or not the flag is set, and re-syncing is a
        // no-op (no spurious ALTER / drift on startup).
        let def = simple_collection(
            "metrics",
            vec![
                FieldDefinition::builder("count", FieldType::Number)
                    .integer(true)
                    .build(),
            ],
        );

        let (_dir, pool) = in_memory_pool();
        let conn = pool.get().unwrap();
        create_collection_table(&conn, "metrics", &def, &no_locale()).unwrap();

        let types = get_table_column_types(&conn, "metrics").unwrap();
        assert_eq!(
            types.get("count").map(String::as_str),
            Some("REAL"),
            "integer-flagged number must stay a REAL column, got {types:?}"
        );

        // Re-syncing the same definition introduces no schema change.
        sync_collection_table(&conn, "metrics", &def, &no_locale()).unwrap();
        let types_after = get_table_column_types(&conn, "metrics").unwrap();
        assert_eq!(types, types_after, "re-sync should not alter the schema");
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
        def.auth = Some(Auth::enabled().map_password_login(|b| b.verify_email(true)));
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
        let col = with_default("name TEXT", FieldType::Text, Some(json!("hello")));
        assert_eq!(col, "name TEXT DEFAULT 'hello'");
    }

    #[test]
    fn append_default_number() {
        let col = with_default("count REAL", FieldType::Number, Some(json!(42)));
        assert_eq!(col, "count REAL DEFAULT 42");
    }

    #[test]
    fn append_default_bool() {
        let col = with_default("active INTEGER", FieldType::Checkbox, Some(json!(true)));
        assert_eq!(col, "active INTEGER DEFAULT 1");
    }

    /// A number default on a checkbox is checked unless it is zero, as a write
    /// of it stores.
    #[test]
    fn append_default_checkbox_number() {
        for (default, literal) in [(json!(2), 1), (json!(0.5), 1), (json!(0), 0)] {
            let col = with_default("active INTEGER", FieldType::Checkbox, Some(default));
            assert_eq!(col, format!("active INTEGER DEFAULT {literal}"));
        }
    }

    #[test]
    fn append_default_checkbox_none() {
        let col = with_default("active INTEGER", FieldType::Checkbox, None);
        assert_eq!(col, "active INTEGER DEFAULT 0");
    }

    #[test]
    fn append_default_none_non_checkbox() {
        let col = with_default("name TEXT", FieldType::Text, None);
        assert_eq!(col, "name TEXT");
    }

    /// A has-many default is the canonical JSON list a write stores.
    #[test]
    fn append_default_has_many_list() {
        let field = FieldDefinition::builder("scores", FieldType::Number)
            .has_many(true)
            .default_value(json!(["1", 2.0]))
            .build();
        let mut col = "scores TEXT".to_string();

        append_default_value_for(&mut col, &field);

        assert_eq!(col, "scores TEXT DEFAULT '[1,2]'");
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

        // Insert two rows with the same slug — a trashed row must not block a
        // live one, which is what the partial managed index expresses.
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

    /// A `unique` field carries no inline `UNIQUE` on a new table: the managed
    /// index `sync_indexes` creates is the one enforcement point, so a field
    /// that gains `unique` later ends up with exactly the same constraint as
    /// one created with it.
    #[test]
    fn unique_field_gets_no_inline_unique() {
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

        let sql = conn
            .query_one(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'posts'",
                &[],
            )
            .unwrap()
            .unwrap()
            .get_string("sql")
            .unwrap();
        assert!(
            !sql.to_uppercase().contains("UNIQUE"),
            "table DDL must carry no inline UNIQUE: {sql}"
        );
    }
}
