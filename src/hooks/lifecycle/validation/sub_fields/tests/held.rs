//! A rich text value a row already holds is accepted unchanged, at every depth.

use serde_json::{Value, json};

use crate::{
    core::{BlockDefinition, DocumentFields, FieldAdmin, FieldDefinition, FieldType},
    db::InMemoryConn,
    hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner},
};

/// A JSON rich text `body` enabling only `bold`: a heading cannot be loaded.
fn body() -> FieldDefinition {
    FieldDefinition::builder("body", FieldType::Richtext)
        .admin(
            FieldAdmin::builder()
                .richtext_format("json")
                .features(vec!["bold".to_string()])
                .build(),
        )
        .build()
}

fn heading(text: &str) -> Value {
    json!({ "type": "doc", "content": [
        { "type": "heading", "attrs": { "level": 1 }, "content": [
            { "type": "text", "text": text }
        ]}
    ]})
}

/// `items[].body` (a join-table column), `content[].meta.body` (a group in a
/// block row's JSON) and `outer[].inner[].body` (an array inside an array
/// row's JSON).
fn fields() -> Vec<FieldDefinition> {
    vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![body()])
            .build(),
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new(
                "section",
                vec![
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![body()])
                        .build(),
                ],
            )])
            .build(),
        FieldDefinition::builder("outer", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("inner", FieldType::Array)
                    .fields(vec![body()])
                    .build(),
            ])
            .build(),
    ]
}

/// A stored document `d1` holding `heading("Kept")` at all three depths.
fn conn() -> InMemoryConn {
    let kept = heading("Kept").to_string();
    let block = json!({ "meta": { "body": heading("Kept") } }).to_string();
    let inner = json!([{ "body": heading("Kept") }]).to_string();

    let conn = InMemoryConn::open();
    conn.setup(&format!(
        "CREATE TABLE test (id TEXT PRIMARY KEY);
         CREATE TABLE test_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, body TEXT);
         CREATE TABLE test_content (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, \
         _block_type TEXT, data TEXT);
         CREATE TABLE test_outer (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, inner TEXT);
         INSERT INTO test VALUES ('d1');
         INSERT INTO test_items VALUES ('i1', 'd1', 0, '{kept}');
         INSERT INTO test_content VALUES ('c1', 'd1', 0, 'section', '{block}');
         INSERT INTO test_outer VALUES ('o1', 'd1', 0, '{inner}');"
    ));
    conn
}

/// A submission carrying `value` at every depth.
fn data(value: &Value) -> DocumentFields {
    [
        ("items".to_string(), json!([{ "body": value }])),
        (
            "content".to_string(),
            json!([{ "_block_type": "section", "meta": { "body": value } }]),
        ),
        (
            "outer".to_string(),
            json!([{ "inner": [{ "body": value }] }]),
        ),
    ]
    .into_iter()
    .collect()
}

/// The fields refused in an update of `d1` submitting `value` everywhere.
fn refused(value: &Value) -> Vec<String> {
    let lua = mlua::Lua::new();
    let conn = conn();
    let ctx = ValidationCtx::builder(&conn, "test")
        .exclude_id(Some("d1"))
        .build();

    let mut refused: Vec<String> = validate_fields_inner(&lua, &fields(), &data(value), &ctx)
        .err()
        .map(|e| e.errors.into_iter().map(|fe| fe.field).collect())
        .unwrap_or_default();
    refused.sort_unstable();

    refused
}

/// Regression: an unloadable value inside a row was refused when resubmitted
/// unchanged — and the admin, which then left it out of the submission, lost
/// it from rows stored as JSON on every save. The held value now passes at
/// every depth, as JSON text or as the object.
#[test]
fn a_held_value_passes_unchanged_at_every_depth() {
    assert!(refused(&heading("Kept")).is_empty());
    assert!(refused(&json!(heading("Kept").to_string())).is_empty());
}

/// A changed value is judged on the check alone, at every depth.
#[test]
fn a_changed_value_is_refused_at_every_depth() {
    assert_eq!(
        refused(&heading("Changed")),
        vec![
            "content[0][meta][0][body]",
            "items[0][body]",
            "outer[0][inner][0][body]",
        ]
    );
}

/// A create holds nothing, so the same value is refused.
#[test]
fn a_create_holds_nothing_in_rows() {
    let lua = mlua::Lua::new();
    let conn = conn();
    let ctx = ValidationCtx::builder(&conn, "test").build();

    let err = validate_fields_inner(&lua, &fields(), &data(&heading("Kept")), &ctx)
        .expect_err("a create cannot hold a value");

    assert_eq!(err.errors.len(), 3);
}
