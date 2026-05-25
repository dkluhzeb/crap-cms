//! Single-container (Row / Collapsible / Tabs / Blocks) inside an array row.

use serde_json::json;

use crate::{
    core::{BlockDefinition, DocumentFields, FieldDefinition, FieldTab, FieldType},
    hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner},
};

#[test]
fn test_validate_collapsible_inside_array_required() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("details", FieldType::Collapsible)
                    .fields(vec![
                        FieldDefinition::builder("note", FieldType::Text)
                            .required(true)
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"note": ""}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Collapsible sub-field inside array should be validated"
    );
    assert!(
        result.unwrap_err().errors[0]
            .field
            .contains("items[0][note]")
    );
}

#[test]
fn test_validate_collapsible_inside_array_date_invalid() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("events", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("info", FieldType::Collapsible)
                    .fields(vec![
                        FieldDefinition::builder("start", FieldType::Date).build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert("events".to_string(), json!([{"start": "not-a-date"}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Invalid date inside collapsible in array should fail"
    );
    assert!(result.unwrap_err().errors[0].message.contains("valid date"));
}

#[test]
fn test_validate_tabs_inside_array_required() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "Content",
                        vec![
                            FieldDefinition::builder("title", FieldType::Text)
                                .required(true)
                                .build(),
                        ],
                    )])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"title": ""}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Required field inside tabs inside array should be validated"
    );
    assert!(
        result.unwrap_err().errors[0]
            .field
            .contains("items[0][title]")
    );
}

#[test]
fn test_validate_tabs_inside_array_date_invalid() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "Meta",
                        vec![FieldDefinition::builder("pub_date", FieldType::Date).build()],
                    )])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"pub_date": "bad-date"}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Invalid date inside tabs inside array should fail"
    );
    assert!(result.unwrap_err().errors[0].message.contains("valid date"));
}

#[test]
fn test_validate_tabs_inside_array_custom_validate() {
    let lua = mlua::Lua::new();
    lua.load(
        r#"
            package.loaded["validators"] = {
                validate_tab_row = function(value, ctx)

                    if value == "bad" then return "tab field error" end

                    return true
                end
            }
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
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "Content",
                        vec![
                            FieldDefinition::builder("slug", FieldType::Text)
                                .validate("validators.validate_tab_row")
                                .build(),
                        ],
                    )])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"slug": "bad"}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Custom validator inside tabs inside array should fire"
    );
    assert!(
        result.unwrap_err().errors[0]
            .message
            .contains("tab field error")
    );
}

#[test]
fn test_validate_row_inside_array_required() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("row", FieldType::Row)
                    .fields(vec![
                        FieldDefinition::builder("label", FieldType::Text)
                            .required(true)
                            .build(),
                    ])
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
    assert!(
        result.is_err(),
        "Required field inside row inside array should be validated"
    );
    assert!(
        result.unwrap_err().errors[0]
            .field
            .contains("items[0][label]")
    );
}

#[test]
fn test_validate_row_inside_array_date_invalid() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("r", FieldType::Row)
                    .fields(vec![
                        FieldDefinition::builder("event_date", FieldType::Date).build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"event_date": "not-a-date"}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Invalid date inside row inside array should fail"
    );
    assert!(result.unwrap_err().errors[0].message.contains("valid date"));
}

#[test]
fn test_validate_row_inside_array_custom_validate() {
    let lua = mlua::Lua::new();
    lua.load(
        r#"
            package.loaded["validators"] = {
                validate_row_field = function(value, ctx)

                    if value == "forbidden" then return "row field forbidden" end

                    return true
                end
            }
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
                FieldDefinition::builder("r", FieldType::Row)
                    .fields(vec![
                        FieldDefinition::builder("code", FieldType::Text)
                            .validate("validators.validate_row_field")
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"code": "forbidden"}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Custom validator inside row inside array should fire"
    );
    assert!(
        result.unwrap_err().errors[0]
            .message
            .contains("row field forbidden")
    );
}

#[test]
fn test_validate_blocks_inside_array_required() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("outer", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("sections", FieldType::Blocks)
                    .blocks(vec![BlockDefinition::new(
                        "heading",
                        vec![
                            FieldDefinition::builder("text", FieldType::Text)
                                .required(true)
                                .build(),
                        ],
                    )])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert(
        "outer".to_string(),
        json!([
            {"sections": [{"_block_type": "heading", "text": ""}]}
        ]),
    );
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Required field inside blocks inside array should be validated"
    );
    assert!(
        result.unwrap_err().errors[0]
            .field
            .contains("outer[0][sections][0][text]")
    );
}

#[test]
fn test_validate_collapsible_inside_array_custom_validate() {
    let lua = mlua::Lua::new();
    lua.load(
        r#"
            package.loaded["validators"] = {
                validate_coll_field = function(value, ctx)

                    if value == "nope" then return "collapsible field rejected" end

                    return true
                end
            }
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
                FieldDefinition::builder("section", FieldType::Collapsible)
                    .fields(vec![
                        FieldDefinition::builder("val", FieldType::Text)
                            .validate("validators.validate_coll_field")
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    let mut data = DocumentFields::new();
    data.insert("items".to_string(), json!([{"val": "nope"}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Custom validator inside collapsible inside array should fire"
    );
    assert!(
        result.unwrap_err().errors[0]
            .message
            .contains("collapsible field rejected")
    );
}

#[test]
fn test_validate_checkbox_inside_array_not_required_when_absent() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("active", FieldType::Checkbox)
                    .required(true)
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
        result.is_ok(),
        "Checkbox inside array should not be required even when required=true"
    );
}
