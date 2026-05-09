//! Draft handling + per-value constraints (length, numeric, email, select).

use serde_json::json;

use crate::{
    core::{
        DocumentFields,
        field::{FieldDefinition, FieldType},
    },
    hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner},
};

#[test]
fn test_validate_array_sub_field_date_format_enforced_in_draft() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();

    // Array sub-field with date format — format should be enforced even in draft mode
    let fields = vec![
        FieldDefinition::builder("events", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("start_date", FieldType::Date).build(),
            ])
            .build(),
    ];

    let mut data = DocumentFields::new();
    data.insert("events".to_string(), json!([{"start_date": "not-a-date"}]));

    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").draft(true).build(),
    );

    assert!(
        result.is_err(),
        "Array sub-field date format should be enforced even in draft mode"
    );
}

#[test]
fn test_validate_array_sub_field_required_skipped_in_draft() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();

    // Array sub-field with required — required should be skipped in draft
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("name", FieldType::Text)
                    .required(true)
                    .build(),
            ])
            .build(),
    ];

    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"name": ""}]));

    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").draft(true).build(),
    );

    assert!(
        result.is_ok(),
        "Array sub-field required check should be skipped in draft mode"
    );
}

// ── Regression tests: sub-field validation checks were missing ────────

/// Regression: length bounds (min_length/max_length) were not enforced
/// inside Array sub-fields — only required/date/custom checks ran.
#[test]
fn test_array_sub_field_max_length_enforced() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();

    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("name", FieldType::Text)
                    .max_length(5)
                    .build(),
            ])
            .build(),
    ];

    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"name": "toolongvalue"}]));

    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );

    assert!(
        result.is_err(),
        "max_length should be enforced in Array sub-fields"
    );
    assert!(
        result.unwrap_err().errors[0]
            .message
            .contains("at most 5 characters")
    );
}

/// Regression: numeric bounds (min/max) were not enforced inside Array sub-fields.
#[test]
fn test_array_sub_field_numeric_bounds_enforced() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();

    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("score", FieldType::Number)
                    .min(0.0)
                    .build(),
            ])
            .build(),
    ];

    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"score": "-5"}]));

    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );

    assert!(
        result.is_err(),
        "min bound should be enforced in Array sub-fields"
    );
    assert!(result.unwrap_err().errors[0].message.contains("at least 0"));
}

/// Regression: email format was not validated inside Array sub-fields.
#[test]
fn test_array_sub_field_email_format_enforced() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();

    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("email", FieldType::Email).build(),
            ])
            .build(),
    ];

    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"email": "not-an-email"}]));

    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );

    assert!(
        result.is_err(),
        "email format should be validated in Array sub-fields"
    );
}

/// Regression: select option validation was not enforced inside Array sub-fields.
#[test]
fn test_array_sub_field_select_option_enforced() {
    use crate::core::DocumentFields;
    use crate::core::field::{LocalizedString, SelectOption};

    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();

    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("color", FieldType::Select)
                    .options(vec![
                        SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
                        SelectOption::new(LocalizedString::Plain("Blue".to_string()), "blue"),
                    ])
                    .build(),
            ])
            .build(),
    ];

    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"color": "invalid_option"}]));

    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );

    assert!(
        result.is_err(),
        "select option validation should be enforced in Array sub-fields"
    );
    assert!(
        result.unwrap_err().errors[0]
            .message
            .contains("invalid option")
    );
}
