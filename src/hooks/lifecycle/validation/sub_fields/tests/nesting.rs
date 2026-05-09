//! Multi-level container nesting (e.g. group inside tabs inside array) plus richtext.

use serde_json::json;

use crate::{
    core::{
        DocumentFields,
        field::{FieldAdmin, FieldDefinition, FieldTab, FieldType},
        registry::Registry,
        richtext::RichtextNodeDef,
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
