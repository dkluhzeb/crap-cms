use std::collections::HashMap;

use serde_json::json;

use crate::{
    core::{
        field::{BlockDefinition, FieldAdmin, FieldDefinition, FieldTab, FieldType},
        registry::Registry,
        richtext::RichtextNodeDef,
    },
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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
    let mut data = HashMap::new();
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

#[test]
fn test_validate_row_inside_tabs_inside_array_required() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    // Array > Tabs > Row > required text (the team_members pattern)
    let fields = vec![
        FieldDefinition::builder("team_members", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("member_tabs", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "Personal",
                        vec![
                            FieldDefinition::builder("name_row", FieldType::Row)
                                .fields(vec![
                                    FieldDefinition::builder("first_name", FieldType::Text)
                                        .required(true)
                                        .build(),
                                    FieldDefinition::builder("last_name", FieldType::Text)
                                        .required(true)
                                        .build(),
                                ])
                                .build(),
                        ],
                    )])
                    .build(),
            ])
            .build(),
    ];
    let mut data = HashMap::new();
    data.insert(
        "team_members".to_string(),
        json!([{"first_name": "", "last_name": ""}]),
    );
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Required field inside Row inside Tabs inside Array should be validated"
    );
    let err = result.unwrap_err();
    assert_eq!(err.errors.len(), 2);
    assert!(err.errors[0].field.contains("team_members[0][first_name]"));
    assert!(err.errors[1].field.contains("team_members[0][last_name]"));
}

#[test]
fn test_validate_group_inside_tabs_inside_array_required() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    // Array > Tabs > Group > required text
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "SEO",
                        vec![
                            FieldDefinition::builder("meta", FieldType::Group)
                                .fields(vec![
                                    FieldDefinition::builder("title", FieldType::Text)
                                        .required(true)
                                        .build(),
                                ])
                                .build(),
                        ],
                    )])
                    .build(),
            ])
            .build(),
    ];
    let mut data = HashMap::new();
    data.insert("items".to_string(), json!([{"meta": {"title": ""}}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Required field inside Group inside Tabs inside Array should be validated"
    );
    assert!(
        result.unwrap_err().errors[0]
            .field
            .contains("items[0][meta][0][title]")
    );
}

#[test]
fn test_validate_collapsible_inside_tabs_inside_array_required() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    // Array > Tabs > Collapsible > required text
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("layout", FieldType::Tabs)
                    .tabs(vec![FieldTab::new(
                        "Advanced",
                        vec![
                            FieldDefinition::builder("extra", FieldType::Collapsible)
                                .fields(vec![
                                    FieldDefinition::builder("note", FieldType::Text)
                                        .required(true)
                                        .build(),
                                ])
                                .build(),
                        ],
                    )])
                    .build(),
            ])
            .build(),
    ];
    let mut data = HashMap::new();
    data.insert("items".to_string(), json!([{"note": ""}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Required field inside Collapsible inside Tabs inside Array should be validated"
    );
    assert!(
        result.unwrap_err().errors[0]
            .field
            .contains("items[0][note]")
    );
}

#[test]
fn test_validate_group_inside_row_inside_array_required() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    // Array > Row > Group > required text
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("r", FieldType::Row)
                    .fields(vec![
                        FieldDefinition::builder("seo", FieldType::Group)
                            .fields(vec![
                                FieldDefinition::builder("title", FieldType::Text)
                                    .required(true)
                                    .build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    let mut data = HashMap::new();
    data.insert("items".to_string(), json!([{"seo": {"title": ""}}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Required field inside Group inside Row inside Array should be validated"
    );
    assert!(
        result.unwrap_err().errors[0]
            .field
            .contains("items[0][seo][0][title]")
    );
}

#[test]
fn test_validate_tabs_inside_collapsible_inside_array_required() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();
    // Array > Collapsible > Tabs > required text
    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("section", FieldType::Collapsible)
                    .fields(vec![
                        FieldDefinition::builder("layout", FieldType::Tabs)
                            .tabs(vec![FieldTab::new(
                                "Content",
                                vec![
                                    FieldDefinition::builder("body", FieldType::Text)
                                        .required(true)
                                        .build(),
                                ],
                            )])
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];
    let mut data = HashMap::new();
    data.insert("items".to_string(), json!([{"body": ""}]));
    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "Required field inside Tabs inside Collapsible inside Array should be validated"
    );
    assert!(
        result.unwrap_err().errors[0]
            .field
            .contains("items[0][body]")
    );
}

#[test]
fn test_validate_richtext_node_attrs_inside_array() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();

    let mut reg = Registry::new();
    reg.register_richtext_node(
        RichtextNodeDef::builder("cta", "CTA")
            .attrs(vec![
                FieldDefinition::builder("text", FieldType::Text)
                    .required(true)
                    .build(),
                FieldDefinition::builder("url", FieldType::Text)
                    .required(true)
                    .build(),
            ])
            .build(),
    );

    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("body", FieldType::Richtext)
                    .admin(
                        FieldAdmin::builder()
                            .nodes(vec!["cta".to_string()])
                            .richtext_format("json")
                            .build(),
                    )
                    .build(),
            ])
            .build(),
    ];

    let json_content = r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":"","url":""}}]}"#;
    let mut data = HashMap::new();
    data.insert("items".to_string(), json!([{"body": json_content}]));

    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").registry(&reg).build(),
    );

    assert!(
        result.is_err(),
        "richtext node attrs should be validated inside array rows"
    );
    let errs = result.unwrap_err().errors;
    assert_eq!(errs.len(), 2);
    assert!(errs[0].field.contains("cta#0"));
    assert!(errs[1].field.contains("cta#0"));
}

#[test]
fn test_validate_richtext_node_attrs_inside_array_draft_skips_required() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();

    let mut reg = Registry::new();
    reg.register_richtext_node(
        RichtextNodeDef::builder("cta", "CTA")
            .attrs(vec![
                FieldDefinition::builder("text", FieldType::Text)
                    .required(true)
                    .build(),
            ])
            .build(),
    );

    let fields = vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("body", FieldType::Richtext)
                    .admin(
                        FieldAdmin::builder()
                            .nodes(vec!["cta".to_string()])
                            .richtext_format("json")
                            .build(),
                    )
                    .build(),
            ])
            .build(),
    ];

    let json_content = r#"{"type":"doc","content":[{"type":"cta","attrs":{"text":""}}]}"#;
    let mut data = HashMap::new();
    data.insert("items".to_string(), json!([{"body": json_content}]));

    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test")
            .registry(&reg)
            .draft(true)
            .build(),
    );

    assert!(
        result.is_ok(),
        "draft mode should skip required checks for richtext node attrs in arrays"
    );
}

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

    let mut data = HashMap::new();
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

    let mut data = HashMap::new();
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
