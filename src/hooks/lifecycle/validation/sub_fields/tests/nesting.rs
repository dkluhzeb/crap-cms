//! Multi-level container nesting (e.g. group inside tabs inside array) plus richtext.

use serde_json::json;

use crate::{
    core::{
        DocumentFields, FieldAdmin, FieldDefinition, FieldTab, FieldType, Registry, RichtextNodeDef,
    },
    hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner},
};

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
    let mut data = DocumentFields::new();
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
    let mut data = DocumentFields::new();
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
    let mut data = DocumentFields::new();
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
    let mut data = DocumentFields::new();
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
    let mut data = DocumentFields::new();
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
    let mut data = DocumentFields::new();
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

/// Regression: `min_rows` / `max_rows` on a nested Array (array-in-array) must
/// be enforced. The top-level walker checked row bounds, but the sub-field
/// walker never called `check_row_bounds`, so a nested array's count limits
/// were silently ignored.
#[test]
fn test_validate_nested_array_min_rows_enforced() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();

    // Array `sections` > Array `items` with min_rows = 2.
    let fields = vec![
        FieldDefinition::builder("sections", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("items", FieldType::Array)
                    .min_rows(2)
                    .fields(vec![
                        FieldDefinition::builder("label", FieldType::Text).build(),
                    ])
                    .build(),
            ])
            .build(),
    ];

    // One section whose nested `items` has only a single row — violates min_rows.
    let mut data = DocumentFields::new();
    data.insert(
        "sections".to_string(),
        json!([{"items": [{"label": "only one"}]}]),
    );

    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );

    assert!(
        result.is_err(),
        "min_rows on a nested array (array-in-array) must be enforced"
    );
    let err = result.unwrap_err();
    assert!(
        err.errors
            .iter()
            .any(|e| e.message.contains("at least 2") && e.field.contains("sections[0][items]")),
        "expected a min_rows error keyed to the nested array, got: {:?}",
        err.errors
    );
}

/// Regression companion: `max_rows` on a Blocks-in-Group sub-field is enforced,
/// and the nested row-bounds check is skipped on draft saves (matching the
/// top-level behavior).
#[test]
fn test_validate_blocks_in_group_max_rows_enforced_and_draft_skipped() {
    let lua = mlua::Lua::new();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
        .unwrap();

    // Array `rows` > Group `cfg` > Array `tags` with max_rows = 1.
    let fields = vec![
        FieldDefinition::builder("rows", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("cfg", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("tags", FieldType::Array)
                            .max_rows(1)
                            .fields(vec![
                                FieldDefinition::builder("name", FieldType::Text).build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];

    // Group's nested `tags` array has two rows — violates max_rows = 1.
    let mut data = DocumentFields::new();
    data.insert(
        "rows".to_string(),
        json!([{"cfg": {"tags": [{"name": "a"}, {"name": "b"}]}}]),
    );

    let result = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").build(),
    );
    assert!(
        result.is_err(),
        "max_rows on an array nested inside a group inside an array must be enforced"
    );
    assert!(
        result
            .unwrap_err()
            .errors
            .iter()
            .any(|e| e.message.contains("at most 1")),
        "expected a max_rows error"
    );

    // Draft saves skip row-bounds, same as the top level.
    let draft = validate_fields_inner(
        &lua,
        &fields,
        &data,
        &ValidationCtx::builder(&conn, "test").draft(true).build(),
    );
    assert!(
        draft.is_ok(),
        "nested row-bounds must be skipped for draft saves"
    );
}
