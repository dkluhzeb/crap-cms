//! Array rows holding a group, a nested array and nested blocks; block rows
//! holding a nested array; block types defining the same names differently;
//! and an array and a has-many relationship inside a group — the fixture both
//! backends' tests share to prove SQL and the in-memory evaluator read every
//! row path alike: a row's own id, a group, a nested array or blocks at any
//! depth, a nested `_block_type`, a checkbox inside a row, a name read per
//! block type, and an array or a has-many relationship inside a group.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::{
    core::{
        BLOCK_TYPE_KEY, BlockDefinition, DocumentFields, FieldDefinition, FieldType,
        RelationshipConfig,
    },
    db::{
        DbConnection, Filter, FilterClause, FilterOp,
        query::filter::{build_where_clause, memory::matches_constraints_typed},
    },
};

fn field(name: &str, field_type: FieldType) -> FieldDefinition {
    FieldDefinition::builder(name, field_type).build()
}

fn text(name: &str) -> FieldDefinition {
    field(name, FieldType::Text)
}

/// `stat` and `note` cards name `score`, `tags` and `info` alike but define
/// them differently: a number vs text, a has-many list vs a single value, a
/// scalar vs a group. Only `stat` has the checkbox `done`; `blank` has none of
/// these fields.
fn cards() -> FieldDefinition {
    let tags = FieldDefinition::builder("tags", FieldType::Text)
        .has_many(true)
        .build();
    let info = FieldDefinition::builder("info", FieldType::Group)
        .fields(vec![text("x")])
        .build();

    FieldDefinition::builder("cards", FieldType::Blocks)
        .blocks(vec![
            BlockDefinition::new(
                "stat",
                vec![
                    field("score", FieldType::Number),
                    tags,
                    text("info"),
                    field("done", FieldType::Checkbox),
                ],
            ),
            BlockDefinition::new("note", vec![text("score"), text("tags"), info]),
            BlockDefinition::new("blank", vec![]),
        ])
        .build()
}

/// A group holding an array and a has-many relationship, each in its own
/// join table.
fn seo() -> FieldDefinition {
    FieldDefinition::builder("seo", FieldType::Group)
        .fields(vec![
            FieldDefinition::builder("links", FieldType::Array)
                .fields(vec![text("url")])
                .build(),
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ])
        .build()
}

/// `items` rows holding a group, a nested array and nested blocks, `content`
/// blocks holding a nested array, the `cards` blocks and the `seo` group.
fn fields() -> Vec<FieldDefinition> {
    let dims = FieldDefinition::builder("dims", FieldType::Group)
        .fields(vec![field("width", FieldType::Number)])
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
        cards(),
        seo(),
    ]
}

/// `(document id, its fields)`, sorted by id, as a read returns them — each
/// top-level row with its own id, the group nested.
fn documents() -> Vec<(&'static str, Value)> {
    vec![
        (
            "p1",
            json!({
                "items": [{
                    "id": "r1",
                    "name": "a",
                    "dims": { "width": 10 },
                    "sizes": [{ "label": "S" }],
                    "parts": [{ "_block_type": "part", "sku": "X1" }],
                }],
                "content": [{ "id": "b1", "_block_type": "image", "caption": "Sunset" }],
                "cards": [{
                    "id": "c1",
                    "_block_type": "stat",
                    "score": 10,
                    "tags": ["a", "b"],
                    "info": "hello",
                    "done": true,
                }],
                "seo": { "links": [{ "id": "l1", "url": "a" }], "tags": ["t1"] },
            }),
        ),
        (
            "p2",
            json!({
                "items": [{ "id": "r2", "name": "b", "sizes": [{ "label": "M" }] }],
                "content": [{ "id": "b2", "_block_type": "talk", "quotes": [{ "who": "Ada" }] }],
                "cards": [
                    {
                        "id": "c2",
                        "_block_type": "note",
                        "score": "high",
                        "tags": "a",
                        "info": { "x": "deep" },
                    },
                    { "id": "c4", "_block_type": "blank" },
                ],
                "seo": { "links": [{ "id": "l2", "url": "b" }], "tags": [] },
            }),
        ),
        (
            "p3",
            json!({
                "items": [],
                "content": [],
                "cards": [{ "id": "c3", "_block_type": "stat", "score": 2, "tags": [], "done": false }],
                "seo": { "links": [], "tags": ["t2"] },
            }),
        ),
    ]
}

/// A SQL literal: the quoted text, or `NULL`.
fn literal(value: Option<String>) -> String {
    value.map_or_else(
        || "NULL".to_string(),
        |text| format!("'{}'", text.replace('\'', "''")),
    )
}

/// A row's column value: text as itself, a container as the JSON text its
/// column stores, a missing or null value as `NULL`.
fn column_of(row: &Value, key: &str) -> Option<String> {
    match row.get(key)? {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

/// The rows `key` holds in `doc` (a missing value holds none).
fn rows_of<'a>(doc: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    doc.get(key).and_then(Value::as_array).into_iter().flatten()
}

/// The `INSERT` of one array row of `parent` into `table`, `columns` after
/// its id and parent.
fn array_insert(table: &str, parent: &str, row: &Value, columns: &[&str]) -> String {
    let mut values = vec![
        literal(column_of(row, "id")),
        literal(Some(parent.to_string())),
    ];

    values.extend(columns.iter().map(|column| literal(column_of(row, column))));

    format!("INSERT INTO \"{table}\" VALUES ({})", values.join(", "))
}

/// The `INSERT` of one block row of `parent` into `table`: its type and id in
/// columns, the rest in `data`, as the write path stores it.
fn block_insert(table: &str, parent: &str, row: &Value) -> String {
    let mut data = row.clone();

    if let Some(object) = data.as_object_mut() {
        object.remove("id");
        object.remove(BLOCK_TYPE_KEY);
    }

    let values = [
        literal(column_of(row, "id")),
        literal(Some(parent.to_string())),
        literal(column_of(row, BLOCK_TYPE_KEY)),
        literal(Some(data.to_string())),
    ];

    format!("INSERT INTO \"{table}\" VALUES ({})", values.join(", "))
}

/// The tables of the fixture under `slug`: the parent, the `items` array,
/// the `content` and `cards` blocks, and the `seo` group's array and junction.
fn create_tables(slug: &str) -> Vec<String> {
    let blocks = "(id TEXT PRIMARY KEY, parent_id TEXT, _block_type TEXT, data TEXT)";

    vec![
        format!(
            "CREATE TABLE \"{slug}\" (id TEXT PRIMARY KEY, _revision INTEGER NOT NULL DEFAULT 0)"
        ),
        format!(
            "CREATE TABLE \"{slug}_items\" (id TEXT PRIMARY KEY, parent_id TEXT, \
             name TEXT, dims TEXT, sizes TEXT, parts TEXT)"
        ),
        format!("CREATE TABLE \"{slug}_content\" {blocks}"),
        format!("CREATE TABLE \"{slug}_cards\" {blocks}"),
        format!(
            "CREATE TABLE \"{slug}_seo__links\" (id TEXT PRIMARY KEY, parent_id TEXT, url TEXT)"
        ),
        format!("CREATE TABLE \"{slug}_seo__tags\" (parent_id TEXT, related_id TEXT)"),
    ]
}

/// The `INSERT`s of one document's rows under `slug`.
fn document_inserts(slug: &str, id: &str, doc: &Value) -> Vec<String> {
    let mut statements = vec![format!("INSERT INTO \"{slug}\" (id) VALUES ('{id}')")];

    let items = format!("{slug}_items");
    let item_columns = ["name", "dims", "sizes", "parts"];
    statements
        .extend(rows_of(doc, "items").map(|row| array_insert(&items, id, row, &item_columns)));

    for blocks in ["content", "cards"] {
        let table = format!("{slug}_{blocks}");
        statements.extend(rows_of(doc, blocks).map(|row| block_insert(&table, id, row)));
    }

    let seo = &doc["seo"];
    let links = format!("{slug}_seo__links");
    statements.extend(rows_of(seo, "links").map(|row| array_insert(&links, id, row, &["url"])));

    for tag in rows_of(seo, "tags").filter_map(Value::as_str) {
        statements.push(format!(
            "INSERT INTO \"{slug}_seo__tags\" VALUES ('{id}', '{tag}')"
        ));
    }

    statements
}

/// Create and fill the fixture's tables under `slug`. Plain literal SQL, so
/// every backend runs it.
fn seed(conn: &dyn DbConnection, slug: &str) {
    let mut statements = create_tables(slug);

    for (id, doc) in documents() {
        statements.extend(document_inserts(slug, id, &doc));
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
        .filter(|(_, doc)| {
            let data: HashMap<String, Value> = doc
                .as_object()
                .map(|object| object.clone().into_iter().collect())
                .unwrap_or_default();

            matches_constraints_typed(&DocumentFields::from(data), filters, &fields)
        })
        .map(|(id, _)| id.to_string())
        .collect()
}

/// A case: the filter path, its operator, and the documents it matches.
type Case = (&'static str, FilterOp, Vec<&'static str>);

/// Row paths through arrays, blocks, groups and nested rows.
fn row_cases() -> Vec<Case> {
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

/// A checkbox inside a block row's JSON, under every operator.
fn checkbox_cases() -> Vec<Case> {
    let done = "cards.done";

    vec![
        (done, FilterOp::Equals("true".into()), vec!["p1"]),
        (done, FilterOp::Equals("false".into()), vec!["p3"]),
        (done, FilterOp::NotEquals("true".into()), vec!["p3"]),
        (done, FilterOp::In(vec!["true".into()]), vec!["p1"]),
        (done, FilterOp::NotIn(vec!["true".into()]), vec!["p3"]),
        (done, FilterOp::Like("1".into()), vec!["p1"]),
        (done, FilterOp::Contains("0".into()), vec!["p3"]),
        (done, FilterOp::GreaterThan("false".into()), vec!["p1"]),
        (done, FilterOp::LessThan("true".into()), vec!["p3"]),
        (
            done,
            FilterOp::GreaterThanOrEqual("0".into()),
            vec!["p1", "p3"],
        ),
        (done, FilterOp::LessThanOrEqual("0".into()), vec!["p3"]),
        (done, FilterOp::Exists, vec!["p1", "p3"]),
        (done, FilterOp::NotExists, vec!["p2"]),
    ]
}

/// Names the `stat` and `note` block types define differently, each read in
/// its own block type's rows; and an array and a has-many relationship
/// inside a group.
fn per_type_and_group_cases() -> Vec<Case> {
    vec![
        // A number in `stat`, text in `note`.
        ("cards.score", FilterOp::Equals("10".into()), vec!["p1"]),
        ("cards.score", FilterOp::Equals("high".into()), vec!["p2"]),
        ("cards.score", FilterOp::LessThan("5".into()), vec!["p3"]),
        // A has-many list in `stat`, a single value in `note`.
        ("cards.tags", FilterOp::Equals("a".into()), vec!["p1", "p2"]),
        ("cards.tags", FilterOp::NotEquals("a".into()), vec!["p3"]),
        // A scalar in `stat`, a group in `note`.
        ("cards.info.x", FilterOp::Equals("deep".into()), vec!["p2"]),
        ("cards.info", FilterOp::Equals("hello".into()), vec!["p1"]),
        ("cards.info", FilterOp::Exists, vec!["p1"]),
        // Negative operators stay inside each block type's reading; a row of a
        // type declaring none of the readings (`blank`) reads the value as
        // absent — NULL, as a name every type defines alike reads there.
        (
            "cards.score",
            FilterOp::NotEquals("10".into()),
            vec!["p2", "p3"],
        ),
        (
            "cards.score",
            FilterOp::NotIn(vec!["10".into()]),
            vec!["p2", "p3"],
        ),
        ("cards.score", FilterOp::NotExists, vec!["p2"]),
        ("cards.score", FilterOp::Exists, vec!["p1", "p2", "p3"]),
        (
            "cards.score",
            FilterOp::NotIn(vec![]),
            vec!["p1", "p2", "p3"],
        ),
        (
            "cards.tags",
            FilterOp::NotIn(vec!["b".into()]),
            vec!["p2", "p3"],
        ),
        ("cards.info", FilterOp::NotEquals("hello".into()), vec![]),
        ("cards.info.x", FilterOp::NotExists, vec!["p2"]),
        ("cards.info.x", FilterOp::Exists, vec!["p2"]),
        // An operand only some readings take: the others match nothing — a
        // word is no number, so `stat` rows never match, however negated.
        ("cards.score", FilterOp::NotEquals("high".into()), vec![]),
        (
            "cards.score",
            FilterOp::NotIn(vec!["10".into(), "high".into()]),
            vec![],
        ),
        // A name one block type defines reads NULL in every other type's rows.
        (
            "cards.done",
            FilterOp::NotIn(vec![]),
            vec!["p1", "p2", "p3"],
        ),
        // An array and a has-many relationship inside a group.
        ("seo.links.url", FilterOp::Equals("a".into()), vec!["p1"]),
        ("seo__links.url", FilterOp::Equals("b".into()), vec!["p2"]),
        ("seo.links.id", FilterOp::In(vec!["l2".into()]), vec!["p2"]),
        ("seo.tags.id", FilterOp::Equals("t1".into()), vec!["p1"]),
        (
            "seo__tags.id",
            FilterOp::NotEquals("t1".into()),
            vec!["p2", "p3"],
        ),
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

    let cases = row_cases()
        .into_iter()
        .chain(checkbox_cases())
        .chain(per_type_and_group_cases());

    for (field, op, expected) in cases {
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
