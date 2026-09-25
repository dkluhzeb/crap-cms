//! A select/radio value a row already holds is accepted unchanged although the
//! field no longer declares it, at every depth.

use serde_json::{Value, json};

use crate::{
    core::{
        BlockDefinition, DocumentFields, FieldDefinition, FieldType, LocalizedString, SelectOption,
    },
    db::InMemoryConn,
    hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner},
};

/// A choice field declaring `values` only.
fn choice(name: &str, field_type: FieldType, has_many: bool, values: &[&str]) -> FieldDefinition {
    let options = values
        .iter()
        .map(|v| SelectOption::new(LocalizedString::Plain(v.to_uppercase()), *v))
        .collect();

    FieldDefinition::builder(name, field_type)
        .has_many(has_many)
        .options(options)
        .build()
}

/// `status` no longer offers `legacy`; `tags` no longer offers `puce`.
fn row_fields() -> Vec<FieldDefinition> {
    vec![
        choice("status", FieldType::Select, false, &["draft", "published"]),
        choice("tags", FieldType::Select, true, &["red", "blue"]),
        choice("size", FieldType::Radio, false, &["sm", "lg"]),
    ]
}

/// The row fields in `items[]` (join-table columns), `content[].meta` (a group
/// in a block row's JSON) and `outer[].inner[]` (an array inside an array row's
/// JSON).
fn fields() -> Vec<FieldDefinition> {
    vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(row_fields())
            .build(),
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new(
                "section",
                vec![
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(row_fields())
                        .build(),
                ],
            )])
            .build(),
        FieldDefinition::builder("outer", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("inner", FieldType::Array)
                    .fields(row_fields())
                    .build(),
            ])
            .build(),
    ]
}

/// The retired values every stored row holds.
fn held_row() -> Value {
    json!({ "status": "legacy", "tags": ["red", "puce"], "size": "xl" })
}

/// A stored document `d1` holding [`held_row`] at all three depths.
fn conn() -> InMemoryConn {
    let block = json!({ "meta": held_row() }).to_string();
    let inner = json!([held_row()]).to_string();

    let conn = InMemoryConn::open();
    conn.setup(&format!(
        "CREATE TABLE test (id TEXT PRIMARY KEY);
         CREATE TABLE test_items (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, \
         status TEXT, tags TEXT, size TEXT);
         CREATE TABLE test_content (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, \
         _block_type TEXT, data TEXT);
         CREATE TABLE test_outer (id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, inner TEXT);
         INSERT INTO test VALUES ('d1');
         INSERT INTO test_items VALUES ('i1', 'd1', 0, 'legacy', '[\"red\",\"puce\"]', 'xl');
         INSERT INTO test_content VALUES ('c1', 'd1', 0, 'section', '{block}');
         INSERT INTO test_outer VALUES ('o1', 'd1', 0, '{inner}');"
    ));
    conn
}

/// A submission carrying `row` at every depth.
fn data(row: &Value) -> DocumentFields {
    [
        ("items".to_string(), json!([row])),
        (
            "content".to_string(),
            json!([{ "_block_type": "section", "meta": row }]),
        ),
        ("outer".to_string(), json!([{ "inner": [row] }])),
    ]
    .into_iter()
    .collect()
}

/// The fields refused in a write submitting `row` everywhere — an update of
/// `d1`, or a create.
fn refused(row: &Value, update: bool) -> Vec<String> {
    let lua = mlua::Lua::new();
    let conn = conn();
    let ctx = ValidationCtx::builder(&conn, "test")
        .exclude_id(update.then_some("d1"))
        .build();

    let mut refused: Vec<String> = validate_fields_inner(&lua, &fields(), &data(row), &ctx)
        .err()
        .map(|e| e.errors.into_iter().map(|fe| fe.field).collect())
        .unwrap_or_default();
    refused.sort_unstable();

    refused
}

/// Regression: a retired option inside a row was refused when resubmitted
/// unchanged — one stored value blocked every later save of the document,
/// though the same value at the top level passed. The held values now pass at
/// every depth, for a single select, a `has_many` select and a radio.
#[test]
fn held_retired_options_pass_unchanged_at_every_depth() {
    assert!(refused(&held_row(), true).is_empty());
}

/// A `has_many` list may be resubmitted as its JSON text too.
#[test]
fn a_held_list_passes_in_its_text_encoding() {
    let row = json!({ "status": "draft", "tags": "[\"puce\"]", "size": "sm" });

    assert!(refused(&row, true).is_empty());
}

/// A value the document does not already hold is still refused at every depth
/// — the rescue is for what the document carries, not a free pass.
#[test]
fn a_newly_chosen_undeclared_value_is_refused_at_every_depth() {
    let row = json!({ "status": "invented", "tags": ["red", "chartreuse"], "size": "sm" });

    assert_eq!(
        refused(&row, true),
        vec![
            "content[0][meta][0][status]",
            "content[0][meta][0][tags]",
            "items[0][status]",
            "items[0][tags]",
            "outer[0][inner][0][status]",
            "outer[0][inner][0][tags]",
        ]
    );
}

/// A value held by one field is not held by another: `legacy` is `status`'s
/// stored value, never `size`'s.
#[test]
fn a_value_is_held_only_by_its_own_field() {
    let row = json!({ "status": "draft", "tags": [], "size": "legacy" });

    assert_eq!(
        refused(&row, true),
        vec![
            "content[0][meta][0][size]",
            "items[0][size]",
            "outer[0][inner][0][size]",
        ]
    );
}

/// A create holds nothing, so the same values are refused.
#[test]
fn a_create_holds_no_retired_option() {
    assert_eq!(refused(&held_row(), false).len(), 9);
}
