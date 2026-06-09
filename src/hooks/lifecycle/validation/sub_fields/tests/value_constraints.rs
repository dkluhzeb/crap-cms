//! Draft handling + per-value constraints (length, numeric, email, select).

use serde_json::json;

use crate::{
    core::{
        DocumentFields, FieldDefinition, FieldType, LocalizedString, RelationshipConfig,
        SelectOption,
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

/// Regression: length bounds (`min_length/max_length`) were not enforced
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

#[test]
fn test_polymorphic_allowlist_enforced_in_array_row() {
    // Regression: a polymorphic relationship nested inside an array row used to
    // bypass the target-collection allowlist (the check ran only at the top
    // level), even though the save path descends into the row. A forged target
    // collection must be rejected here exactly as at the top level.
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();

    let mut rc = RelationshipConfig::new("", false);
    rc.polymorphic = vec!["posts".into()];
    let fields = vec![
        FieldDefinition::builder("blocks", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("target", FieldType::Relationship)
                    .relationship(rc)
                    .build(),
            ])
            .build(),
    ];

    let mut data = DocumentFields::new();
    data.insert("blocks".to_string(), json!([{"target": "secret/s1"}]));

    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );

    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.message.contains("secret") && e.message.contains("polymorphic allowlist")),
        "polymorphic allowlist must be enforced inside array rows, got: {:?}",
        err.errors
    );
}

#[test]
fn test_nested_array_non_object_row_is_reported() {
    // Regression: a non-object row in a nested array/blocks list was silently
    // skipped (`continue`), unlike the top-level walker which emits
    // `invalid_row_type`. A malformed deeply-nested row must surface an error.
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();

    // Array > Array (array-in-array); the inner array holds a bare string.
    let fields = vec![
        FieldDefinition::builder("outer", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("inner", FieldType::Array)
                    .fields(vec![
                        FieldDefinition::builder("value", FieldType::Text).build(),
                    ])
                    .build(),
            ])
            .build(),
    ];

    let mut data = DocumentFields::new();
    data.insert("outer".to_string(), json!([{"inner": ["not-an-object"]}]));

    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );

    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.message.contains("must be an object")),
        "non-object nested row must surface an error, got: {:?}",
        err.errors
    );
}
