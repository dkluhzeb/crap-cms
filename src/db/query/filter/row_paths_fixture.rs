//! Array rows holding a group, a nested array and nested blocks, and block
//! rows holding a nested array — the fixture both backends' tests share to
//! prove SQL and the in-memory evaluator read every row path alike: a row's own
//! id, a group, a nested array or blocks at any depth, a nested `_block_type`.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::{
    core::{BLOCK_TYPE_KEY, BlockDefinition, DocumentFields, FieldDefinition, FieldType},
    db::{
        DbConnection, Filter, FilterClause, FilterOp,
        query::filter::{build_where_clause, memory::matches_constraints_typed},
    },
};

fn text(name: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Text).build()
}

/// `items` rows holding a group, a nested array and nested blocks, and
/// `content` blocks holding a nested array.
fn fields() -> Vec<FieldDefinition> {
    let dims = FieldDefinition::builder("dims", FieldType::Group)
        .fields(vec![
            FieldDefinition::builder("width", FieldType::Number).build(),
        ])
        .build();
    let sizes = FieldDefinition::builder("sizes", FieldType::Array)
        .fields(vec![text("label")])
        .build();
    let parts = FieldDefinition::builder("parts", FieldType::Blocks)
        .blocks(vec![BlockDefinition::new("part", vec![text("sku")])])
        .build();
    let quotes = FieldDefinition::builder("quotes", FieldType::Array)
        .fields(vec![text("who")])
        .build();

    vec![
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![text("name"), dims, sizes, parts])
            .build(),
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![
                BlockDefinition::new("image", vec![text("caption")]),
                BlockDefinition::new("talk", vec![quotes]),
            ])
            .build(),
    ]
}

/// `(document id, its items rows, its content rows)`, sorted by id, as a read
/// returns them — each top-level row with its own id.
fn documents() -> Vec<(&'static str, Value, Value)> {
    vec![
        (
            "p1",
            json!([{
                "id": "r1",
                "name": "a",
                "dims": { "width": 10 },
                "sizes": [{ "label": "S" }],
                "parts": [{ "_block_type": "part", "sku": "X1" }],
            }]),
            json!([{ "id": "b1", "_block_type": "image", "caption": "Sunset" }]),
        ),
        (
            "p2",
            json!([{ "id": "r2", "name": "b", "sizes": [{ "label": "M" }] }]),
            json!([{ "id": "b2", "_block_type": "talk", "quotes": [{ "who": "Ada" }] }]),
        ),
        ("p3", json!([]), json!([])),
    ]
}

/// A SQL literal: the quoted text, or `NULL`.
fn literal(value: Option<String>) -> String {
    value.map_or_else(
        || "NULL".to_string(),
        |text| format!("'{}'", text.replace('\'', "''")),
    )
}

/// A row's text value.
fn text_of(row: &Value, key: &str) -> Option<String> {
    row.get(key).and_then(Value::as_str).map(str::to_string)
}

/// A row's container value, as the JSON text its column stores.
fn json_of(row: &Value, key: &str) -> Option<String> {
    row.get(key).map(Value::to_string)
}

/// The `INSERT` of one array row of `parent`.
fn item_insert(slug: &str, parent: &str, row: &Value) -> String {
    let values = [
        literal(text_of(row, "id")),
        literal(Some(parent.to_string())),
        literal(text_of(row, "name")),
        literal(json_of(row, "dims")),
        literal(json_of(row, "sizes")),
        literal(json_of(row, "parts")),
    ];

    format!(
        "INSERT INTO \"{slug}_items\" VALUES ({})",
        values.join(", ")
    )
}

/// The `INSERT` of one block row of `parent`: its type and id in columns, the
/// rest in `data`, as the write path stores it.
fn block_insert(slug: &str, parent: &str, row: &Value) -> String {
    let mut data = row.clone();

    if let Some(object) = data.as_object_mut() {
        object.remove("id");
        object.remove(BLOCK_TYPE_KEY);
    }

    let values = [
        literal(text_of(row, "id")),
        literal(Some(parent.to_string())),
        literal(text_of(row, BLOCK_TYPE_KEY)),
        literal(Some(data.to_string())),
    ];

    format!(
        "INSERT INTO \"{slug}_content\" VALUES ({})",
        values.join(", ")
    )
}

/// Create and fill the parent table `slug`, its `items` array table and its
/// `content` blocks table. Plain literal SQL, so every backend runs it.
fn seed(conn: &dyn DbConnection, slug: &str) {
    let mut statements = vec![
        format!("CREATE TABLE \"{slug}\" (id TEXT PRIMARY KEY)"),
        format!(
            "CREATE TABLE \"{slug}_items\" (id TEXT PRIMARY KEY, parent_id TEXT, \
             name TEXT, dims TEXT, sizes TEXT, parts TEXT)"
        ),
        format!(
            "CREATE TABLE \"{slug}_content\" (id TEXT PRIMARY KEY, parent_id TEXT, \
             _block_type TEXT, data TEXT)"
        ),
    ];

    for (id, items, content) in documents() {
        statements.push(format!("INSERT INTO \"{slug}\" (id) VALUES ('{id}')"));

        for row in items.as_array().into_iter().flatten() {
            statements.push(item_insert(slug, id, row));
        }

        for row in content.as_array().into_iter().flatten() {
            statements.push(block_insert(slug, id, row));
        }
    }

    for sql in statements {
        conn.execute_ddl(&sql, &[]).unwrap();
    }
}

/// The documents SQL matches for `filters`, sorted.
fn sql_ids(conn: &dyn DbConnection, slug: &str, filters: &[FilterClause]) -> Vec<String> {
    let mut params = Vec::new();
    let clause = build_where_clause(conn, filters, slug, &fields(), None, &mut params).unwrap();

    conn.query_all(
        &format!("SELECT id FROM \"{slug}\"{clause} ORDER BY id"),
        &params,
    )
    .unwrap()
    .iter()
    .map(|row| row.get_string("id").unwrap())
    .collect()
}

/// The documents the in-memory evaluator matches for `filters`.
fn memory_ids(filters: &[FilterClause]) -> Vec<String> {
    let fields = fields();

    documents()
        .into_iter()
        .filter(|(_, items, content)| {
            let doc = DocumentFields::from(HashMap::from([
                ("items".to_string(), items.clone()),
                ("content".to_string(), content.clone()),
            ]));

            matches_constraints_typed(&doc, filters, &fields)
        })
        .map(|(id, _, _)| id.to_string())
        .collect()
}

/// Every row path of the fixture, with the documents it matches.
fn cases() -> Vec<(&'static str, FilterOp, Vec<&'static str>)> {
    vec![
        ("items.id", FilterOp::Equals("r1".into()), vec!["p1"]),
        ("items.id", FilterOp::NotEquals("r1".into()), vec!["p2"]),
        ("content.id", FilterOp::In(vec!["b2".into()]), vec!["p2"]),
        (
            "items.dims.width",
            FilterOp::GreaterThan("9".into()),
            vec!["p1"],
        ),
        (
            "items.sizes.label",
            FilterOp::Equals("M".into()),
            vec!["p2"],
        ),
        ("items.sizes.label", FilterOp::Like("s".into()), vec!["p1"]),
        ("items.parts.sku", FilterOp::Equals("X1".into()), vec!["p1"]),
        (
            "items.parts._block_type",
            FilterOp::Equals("part".into()),
            vec!["p1"],
        ),
        (
            "content.quotes.who",
            FilterOp::Equals("Ada".into()),
            vec!["p2"],
        ),
        ("content.caption", FilterOp::Exists, vec!["p1"]),
    ]
}

/// Seed the fixture under `slug` on `conn`, and assert that SQL and the
/// in-memory evaluator both match exactly the expected documents for every
/// row path. The caller drops the `slug` tables afterwards where they outlive
/// the connection.
///
/// # Panics
///
/// Panics when seeding fails, or when either side disagrees with a case.
pub(crate) fn assert_row_paths_agree(conn: &dyn DbConnection, slug: &str) {
    seed(conn, slug);

    for (field, op, expected) in cases() {
        let filters = vec![FilterClause::Single(Filter {
            field: field.to_string(),
            op: op.clone(),
        })];
        let expected: Vec<String> = expected.iter().map(ToString::to_string).collect();

        assert_eq!(
            sql_ids(conn, slug, &filters),
            expected,
            "sql {field} {op:?}"
        );
        assert_eq!(memory_ids(&filters), expected, "memory {field} {op:?}");
    }
}
