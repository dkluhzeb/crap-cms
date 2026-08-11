//! Array + Blocks fundamental sub-field validation.

use serde_json::json;

use crate::{
    core::{BlockDefinition, DocumentFields, FieldDefinition, FieldType},
    hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner},
};

#[test]
fn test_validate_array_sub_field_required() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("label", FieldType::Text)
                    .required(true)
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"label": ""}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.errors[0].field.contains("items[0][label]"));
}

#[test]
fn test_validate_blocks_sub_field_required() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new(
                "text",
                vec![
                    FieldDefinition::builder("body", FieldType::Text)
                        .required(true)
                        .build(),
                ],
            )])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert(
        "content".to_string(),
        json!([{"_block_type": "text", "body": ""}]),
    );
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(result.is_err());
    assert!(
        result.unwrap_err().errors[0]
            .field
            .contains("content[0][body]")
    );
}

#[test]
fn test_validate_nested_array_in_array() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("outer", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("inner", FieldType::Array)
                    .fields(vec![
                        FieldDefinition::builder("value", FieldType::Text)
                            .required(true)
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert(
        "outer".to_string(),
        json!([
            {"inner": [{"value": ""}]}
        ]),
    );
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.errors[0].field.contains("outer[0][inner][0][value]"));
}

#[test]
fn test_validate_group_inside_array() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("title", FieldType::Text)
                            .required(true)
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert(
        "items".to_string(),
        json!([
            {"meta": {"title": ""}}
        ]),
    );
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.errors[0].field.contains("items[0][meta][0][title]"));
}

#[test]
fn test_validate_date_inside_array_subfield() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("events", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("date", FieldType::Date).build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert(
        "events".to_string(),
        json!([
            {"date": "not-a-date"}
        ]),
    );
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().errors[0].message.contains("valid date"));
}

#[test]
fn test_validate_custom_validate_in_array_subfield() {
    let lua = mlua::Lua::new();
    lua.load(
        r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_sub = function(value, ctx)

                if value == "invalid" then

                    return "sub-field invalid"
                end

                return true
            end
        "#,
    )
    .exec()
    .unwrap();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("val", FieldType::Text)
                    .validate("validators.validate_sub")
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert(
        "items".to_string(),
        json!([
            {"val": "invalid"}
        ]),
    );
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(result.is_err());
    assert!(
        result.unwrap_err().errors[0]
            .message
            .contains("sub-field invalid")
    );
}

#[test]
fn test_validate_date_in_group_inside_array() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("publish_date", FieldType::Date).build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert(
        "items".to_string(),
        json!([
            {"meta": {"publish_date": "bad-date"}}
        ]),
    );
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().errors[0].message.contains("valid date"));
}

#[test]
fn test_validate_custom_function_in_group_inside_array() {
    let lua = mlua::Lua::new();
    lua.load(
        r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_group_sub = function(value, ctx)

                return "group validation error"
            end
        "#,
    )
    .exec()
    .unwrap();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("slug", FieldType::Text)
                            .validate("validators.validate_group_sub")
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert(
        "items".to_string(),
        json!([
            {"meta": {"slug": "test-slug"}}
        ]),
    );
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(result.is_err());
    assert!(
        result.unwrap_err().errors[0]
            .message
            .contains("group validation error")
    );
}

#[test]
fn test_validate_array_sub_field_skipped_for_draft() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("label", FieldType::Text)
                    .required(true)
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"label": ""}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").draft(true).build(),
    );
    assert!(
        result.is_ok(),
        "Array sub-field required check should be skipped for drafts"
    );
}

/// Regression: unknown block types must produce a validation error, not be
/// silently skipped — otherwise blocks with arbitrary types bypass all
/// field validation.
#[test]
fn test_validate_blocks_unknown_block_type_rejected() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new(
                "text",
                vec![
                    FieldDefinition::builder("body", FieldType::Text)
                        .required(true)
                        .build(),
                ],
            )])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert(
        "content".to_string(),
        json!([{"_block_type": "image", "url": ""}]),
    );
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(result.is_err(), "Unknown block type must be rejected");
    let err = result.unwrap_err();
    assert!(
        err.errors[0].message.contains("unknown block type"),
        "error message should mention unknown block type: {}",
        err.errors[0].message,
    );
}

/// Regression: the same rejection must apply to blocks NESTED inside an
/// array/blocks row — `validate_nested_rows` silently skipped unknown
/// block types while the top-level walker errored.
#[test]
fn nested_blocks_unknown_block_type_rejected() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("sections", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("content", FieldType::Blocks)
                    .blocks(vec![BlockDefinition::new(
                        "text",
                        vec![FieldDefinition::builder("body", FieldType::Text).build()],
                    )])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert(
        "sections".to_string(),
        json!([{"content": [{"_block_type": "bogus", "x": 1}]}]),
    );
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Unknown block type in a nested blocks row must be rejected"
    );
    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.key.as_deref() == Some("validation.unknown_block_type")),
        "expected unknown_block_type, got: {:?}",
        err.errors
    );
}

/// Regression: a row that omits a group (or sends it as a non-object)
/// skipped ALL child validation — a required sub-field inside the group
/// never fired. An absent group must validate its children as absent.
#[test]
fn missing_group_in_row_enforces_required_children() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("meta", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("title", FieldType::Text)
                            .required(true)
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "row without the group must still fail the group's required child"
    );

    // A wrong-typed group value is a malformed row, not a skip.
    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"meta": "not-an-object"}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(result.is_err(), "non-object group value must be rejected");
}

/// Regression: a nested required Array/Blocks sub-field given `[]` passed
/// `required` (top level demands a non-empty array for join-shaped fields).
#[test]
fn nested_required_array_rejects_empty_array() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("outer", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("inner", FieldType::Array)
                    .required(true)
                    .fields(vec![FieldDefinition::builder("v", FieldType::Text).build()])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert("outer".to_string(), json!([{"inner": []}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "empty array must not satisfy required on a nested array sub-field"
    );
    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.key.as_deref() == Some("validation.required")),
        "expected a required error, got: {:?}",
        err.errors
    );
}

/// Regression: non-object rows in an array field must produce a validation
/// error — primitives should not silently bypass sub-field validation.
#[test]
fn test_validate_array_non_object_rows_rejected() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("label", FieldType::Text)
                    .required(true)
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!(["plain-string", 42, null]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(result.is_err(), "Non-object array rows must be rejected");
    let err = result.unwrap_err();
    assert_eq!(
        err.errors.len(),
        3,
        "each non-object row should produce an error"
    );
    assert!(
        err.errors[0].message.contains("must be an object"),
        "error message should mention object requirement: {}",
        err.errors[0].message,
    );
}
