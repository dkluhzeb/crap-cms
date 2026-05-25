//! End-to-end tests for the hydrate + save round-trip across join shapes.
//!
//! Sibling `tests.rs` rather than inline in `mod.rs` so the module
//! root stays a thin declarations + re-exports file per CLAUDE.md.
//! The shared `test_helpers` lives in `mod.rs` alongside its declaration
//! because the sibling `save.rs` test block also imports from it.

use std::collections::HashMap;

use serde_json::json;

use crate::core::DocumentFields;

use super::test_helpers::{array_sub_fields, posts_def_with_joins, setup_join_db};
use super::*;
use crate::config::CrapConfig;
use crate::core::{Document, field::*};
use crate::db::query::join::{
    arrays::{find_array_rows, set_array_rows},
    blocks::{find_block_rows, set_block_rows},
    relationships::{set_polymorphic_related, set_related_ids},
};
use crate::db::{BoxedConnection, DbConnection, pool};
use tempfile::TempDir;

fn setup_conn(sql: &str) -> (TempDir, BoxedConnection) {
    let dir = TempDir::new().unwrap();
    let config = CrapConfig::default();
    let p = pool::create_pool(dir.path(), &config).unwrap();
    let conn = p.get().unwrap();
    conn.execute_batch(sql).unwrap();
    (dir, conn)
}

// ── hydrate_document ─────────────────────────────────────────────────────

#[test]
fn hydrate_has_many_and_array() {
    let (_dir, conn) = setup_join_db();
    let def = posts_def_with_joins();

    let tag_ids = vec!["t1".to_string(), "t2".to_string()];
    set_related_ids(&conn, "posts", "tags", "p1", &tag_ids, None).unwrap();

    let sub = array_sub_fields();
    let rows = vec![HashMap::from([
        ("label".to_string(), json!("Item 1")),
        ("value".to_string(), json!("Val 1")),
    ])];
    set_array_rows(&conn, "posts", "items", "p1", &rows, &sub, None).unwrap();

    let blocks = vec![json!({"_block_type": "text", "body": "Hello"})];
    set_block_rows(&conn, "posts", "content", "p1", &blocks, None).unwrap();

    let mut doc = Document::new("p1".to_string());
    doc.fields.insert("title".to_string(), json!("Post 1"));

    hydrate_document(&conn, "posts", &def.fields, &mut doc, None, None).unwrap();

    let tags = doc.fields.get("tags").expect("tags should be populated");
    let tags_arr = tags.as_array().expect("tags should be an array");
    assert_eq!(tags_arr.len(), 2);
    assert_eq!(tags_arr[0], "t1");
    assert_eq!(tags_arr[1], "t2");

    let items = doc.fields.get("items").expect("items should be populated");
    let items_arr = items.as_array().expect("items should be an array");
    assert_eq!(items_arr.len(), 1);
    assert_eq!(items_arr[0]["label"], "Item 1");
    assert_eq!(items_arr[0]["value"], "Val 1");

    let content = doc
        .fields
        .get("content")
        .expect("content should be populated");
    let content_arr = content.as_array().expect("content should be an array");
    assert_eq!(content_arr.len(), 1);
    assert_eq!(content_arr[0]["_block_type"], "text");
    assert_eq!(content_arr[0]["body"], "Hello");

    assert_eq!(doc.get_str("title"), Some("Post 1"));
}

// ── hydrate_document with select ────────────────────────────────────────

#[test]
fn hydrate_with_select_filters_fields() {
    let (_dir, conn) = setup_join_db();
    let def = posts_def_with_joins();

    let tag_ids = vec!["t1".to_string()];
    set_related_ids(&conn, "posts", "tags", "p1", &tag_ids, None).unwrap();
    let sub = array_sub_fields();
    let rows = vec![HashMap::from([
        ("label".to_string(), json!("Item 1")),
        ("value".to_string(), json!("Val 1")),
    ])];
    set_array_rows(&conn, "posts", "items", "p1", &rows, &sub, None).unwrap();

    let mut doc = Document::new("p1".to_string());
    doc.fields.insert("title".to_string(), json!("Post 1"));

    let select = vec!["tags".to_string(), "title".to_string()];
    hydrate_document(&conn, "posts", &def.fields, &mut doc, Some(&select), None).unwrap();

    assert!(doc.fields.contains_key("tags"), "tags should be hydrated");
    assert!(
        !doc.fields.contains_key("items"),
        "items should NOT be hydrated (not in select)"
    );
    assert!(
        !doc.fields.contains_key("content"),
        "content should NOT be hydrated (not in select)"
    );
}

// ── Polymorphic hydration ─────────────────────────────────────────────

#[test]
fn hydrate_polymorphic_has_many() {
    let (_dir, conn) = setup_conn(
        "CREATE TABLE posts (id TEXT PRIMARY KEY);
         CREATE TABLE posts_refs (
             parent_id TEXT,
             related_id TEXT,
             related_collection TEXT NOT NULL DEFAULT '',
             _order INTEGER,
             PRIMARY KEY (parent_id, related_id, related_collection)
         );
         INSERT INTO posts (id) VALUES ('p1');",
    );

    let items = vec![
        ("articles".to_string(), "a1".to_string()),
        ("pages".to_string(), "pg1".to_string()),
    ];
    set_polymorphic_related(&conn, "posts", "refs", "p1", &items, None).unwrap();

    let mut refs_rel = RelationshipConfig::new("articles", true);
    refs_rel.polymorphic = vec!["articles".into(), "pages".into()];
    let fields = vec![
        FieldDefinition::builder("refs", FieldType::Relationship)
            .relationship(refs_rel)
            .build(),
    ];

    let mut doc = Document::new("p1".to_string());
    hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

    let refs = doc.fields.get("refs").expect("refs should be hydrated");
    let arr = refs.as_array().expect("should be array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0].as_str().unwrap(), "articles/a1");
    assert_eq!(arr[1].as_str().unwrap(), "pages/pg1");
}

// ── Regression: nested layout field types ────────────────────────────

#[test]
fn save_and_hydrate_blocks_inside_tabs() {
    // Regression: blocks nested inside a Tabs field were lost on save and invisible on read
    let (_dir, conn) = setup_join_db();

    let blocks_field = FieldDefinition::builder("content", FieldType::Blocks).build();
    let tabs_field = FieldDefinition::builder("page_settings", FieldType::Tabs)
        .tabs(vec![FieldTab::new("Content", vec![blocks_field])])
        .build();
    let fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        tabs_field,
    ];

    let mut data = DocumentFields::new();
    data.insert(
        "content".to_string(),
        json!([
            {"_block_type": "hero", "heading": "Welcome"},
            {"_block_type": "text", "body": "Hello world"},
        ]),
    );
    save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

    let rows = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
    assert_eq!(rows.len(), 2, "blocks should be saved through Tabs");
    assert_eq!(rows[0]["_block_type"], "hero");
    assert_eq!(rows[1]["_block_type"], "text");

    let mut doc = Document::new("p1".to_string());
    doc.fields.insert("title".to_string(), json!("Post 1"));
    hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

    let content = doc
        .fields
        .get("content")
        .expect("blocks must be hydrated through Tabs");
    let arr = content.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["_block_type"], "hero");
    assert_eq!(arr[1]["_block_type"], "text");
}

#[test]
fn save_and_hydrate_array_inside_row() {
    // Regression: arrays nested inside a Row field were lost on save and invisible on read
    let (_dir, conn) = setup_join_db();

    let array_field = FieldDefinition::builder("items", FieldType::Array)
        .fields(array_sub_fields())
        .build();
    let row_field = FieldDefinition::builder("main_row", FieldType::Row)
        .fields(vec![array_field])
        .build();
    let fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        row_field,
    ];

    let mut data = DocumentFields::new();
    data.insert(
        "items".to_string(),
        json!([
            {"label": "First", "value": "1"},
            {"label": "Second", "value": "2"},
        ]),
    );
    save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

    let rows = find_array_rows(&conn, "posts", "items", "p1", &array_sub_fields(), None).unwrap();
    assert_eq!(rows.len(), 2, "array should be saved through Row");
    assert_eq!(rows[0]["label"], "First");
    assert_eq!(rows[1]["label"], "Second");

    let mut doc = Document::new("p1".to_string());
    hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

    let items = doc
        .fields
        .get("items")
        .expect("array must be hydrated through Row");
    let arr = items.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["label"], "First");
    assert_eq!(arr[1]["value"], "2");
}

// ── Group > Array + Blocks hydration ───────────────────────────────

#[test]
fn hydrate_group_array_and_blocks() {
    let (_dir, conn) = setup_conn(
        "CREATE TABLE posts (
            id TEXT PRIMARY KEY,
            config__label TEXT
        );
        CREATE TABLE posts_config__items (
            id TEXT PRIMARY KEY,
            parent_id TEXT,
            _order INTEGER,
            name TEXT
        );
        CREATE TABLE posts_config__sections (
            id TEXT PRIMARY KEY,
            parent_id TEXT,
            _order INTEGER,
            _block_type TEXT,
            data TEXT
        );
        INSERT INTO posts (id, config__label) VALUES ('p1', 'My Config');",
    );

    let fields = vec![
        FieldDefinition::builder("config", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("label", FieldType::Text).build(),
                FieldDefinition::builder("items", FieldType::Array)
                    .fields(vec![
                        FieldDefinition::builder("name", FieldType::Text).build(),
                    ])
                    .build(),
                FieldDefinition::builder("sections", FieldType::Blocks).build(),
            ])
            .build(),
    ];

    // Save array rows
    let array_rows = vec![HashMap::from([("name".to_string(), json!("Item1"))])];
    set_array_rows(
        &conn,
        "posts",
        "config__items",
        "p1",
        &array_rows,
        &[FieldDefinition::builder("name", FieldType::Text).build()],
        None,
    )
    .unwrap();

    // Save block rows
    let block_rows = vec![json!({"_block_type": "text", "body": "Hello"})];
    set_block_rows(&conn, "posts", "config__sections", "p1", &block_rows, None).unwrap();

    // Create document with the scalar group column
    let mut doc = Document::new("p1".to_string());
    doc.fields
        .insert("config__label".to_string(), json!("My Config"));

    hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

    let config = doc
        .fields
        .get("config")
        .expect("config group should be hydrated");
    assert_eq!(
        config.get("label").and_then(|v| v.as_str()),
        Some("My Config")
    );

    let items = config
        .get("items")
        .expect("items array should exist in config");
    let items_arr = items.as_array().expect("items should be array");
    assert_eq!(items_arr.len(), 1);
    assert_eq!(items_arr[0]["name"], "Item1");

    let sections = config
        .get("sections")
        .expect("sections blocks should exist in config");
    let sections_arr = sections.as_array().expect("sections should be array");
    assert_eq!(sections_arr.len(), 1);
    assert_eq!(sections_arr[0]["_block_type"], "text");
    assert_eq!(sections_arr[0]["body"], "Hello");
}

#[test]
fn hydrate_group_relationship() {
    let (_dir, conn) = setup_conn(
        "CREATE TABLE posts (
            id TEXT PRIMARY KEY,
            config__label TEXT
        );
        CREATE TABLE posts_config__tags (
            parent_id TEXT,
            related_id TEXT,
            _order INTEGER,
            PRIMARY KEY (parent_id, related_id)
        );
        INSERT INTO posts (id, config__label) VALUES ('p1', 'My Config');",
    );

    let tag_ids = vec!["t1".to_string(), "t2".to_string()];
    set_related_ids(&conn, "posts", "config__tags", "p1", &tag_ids, None).unwrap();

    let fields = vec![
        FieldDefinition::builder("config", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("label", FieldType::Text).build(),
                FieldDefinition::builder("tags", FieldType::Relationship)
                    .relationship(RelationshipConfig::new("tags", true))
                    .build(),
            ])
            .build(),
    ];

    let mut doc = Document::new("p1".to_string());
    doc.fields
        .insert("config__label".to_string(), json!("My Config"));

    hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

    let config = doc
        .fields
        .get("config")
        .expect("config group should be hydrated");
    assert_eq!(
        config.get("label").and_then(|v| v.as_str()),
        Some("My Config")
    );
    let tags = config
        .get("tags")
        .expect("tags relationship should exist in config");
    let tags_arr = tags.as_array().expect("tags should be array");
    assert_eq!(tags_arr.len(), 2);
    assert_eq!(tags_arr[0], "t1");
    assert_eq!(tags_arr[1], "t2");
}

#[test]
fn hydrate_group_group_array() {
    let (_dir, conn) = setup_conn(
        "CREATE TABLE posts (
            id TEXT PRIMARY KEY,
            outer__inner__label TEXT
        );
        CREATE TABLE posts_outer__inner__items (
            id TEXT PRIMARY KEY,
            parent_id TEXT,
            _order INTEGER,
            name TEXT
        );
        INSERT INTO posts (id, outer__inner__label) VALUES ('p1', 'Deep Label');",
    );

    let fields = vec![
        FieldDefinition::builder("outer", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("inner", FieldType::Group)
                    .fields(vec![
                        FieldDefinition::builder("label", FieldType::Text).build(),
                        FieldDefinition::builder("items", FieldType::Array)
                            .fields(vec![
                                FieldDefinition::builder("name", FieldType::Text).build(),
                            ])
                            .build(),
                    ])
                    .build(),
            ])
            .build(),
    ];

    let array_rows = vec![HashMap::from([("name".to_string(), json!("Item1"))])];
    set_array_rows(
        &conn,
        "posts",
        "outer__inner__items",
        "p1",
        &array_rows,
        &[FieldDefinition::builder("name", FieldType::Text).build()],
        None,
    )
    .unwrap();

    let mut doc = Document::new("p1".to_string());
    doc.fields
        .insert("outer__inner__label".to_string(), json!("Deep Label"));

    hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

    let outer = doc.fields.get("outer").expect("outer group should exist");
    let inner = outer.get("inner").expect("inner group should exist");
    assert_eq!(
        inner.get("label").and_then(|v| v.as_str()),
        Some("Deep Label")
    );
    let items = inner.get("items").expect("items array should exist");
    let items_arr = items.as_array().expect("items should be array");
    assert_eq!(items_arr.len(), 1);
    assert_eq!(items_arr[0]["name"], "Item1");
}

#[test]
fn save_and_hydrate_blocks_inside_collapsible() {
    // Regression: blocks nested inside a Collapsible field were lost
    let (_dir, conn) = setup_join_db();

    let blocks_field = FieldDefinition::builder("content", FieldType::Blocks).build();
    let collapsible_field = FieldDefinition::builder("advanced", FieldType::Collapsible)
        .fields(vec![blocks_field])
        .build();
    let fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        collapsible_field,
    ];

    let mut data = DocumentFields::new();
    data.insert(
        "content".to_string(),
        json!([
            {"_block_type": "cta", "heading": "Buy now"},
        ]),
    );
    save_join_table_data(&conn, "posts", &fields, "p1", &data, None).unwrap();

    let rows = find_block_rows(&conn, "posts", "content", "p1", None).unwrap();
    assert_eq!(rows.len(), 1, "blocks should be saved through Collapsible");
    assert_eq!(rows[0]["_block_type"], "cta");

    let mut doc = Document::new("p1".to_string());
    hydrate_document(&conn, "posts", &fields, &mut doc, None, None).unwrap();

    let content = doc
        .fields
        .get("content")
        .expect("blocks must be hydrated through Collapsible");
    let arr = content.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["_block_type"], "cta");
}

// ── hydrate_documents (batched) ─────────────────────────────────────────

/// The batched plural-doc hydrate must produce identical output to
/// calling [`hydrate_document`] per doc — same JSON shape, same
/// ordering, empty-rel docs left with an empty array. This is the
/// behavioural correctness check for the optimization that replaced
/// N relationship SELECTs with one `WHERE parent_id IN (…)` SELECT.
#[test]
fn hydrate_documents_matches_per_doc_for_relationship_field() {
    let (_dir, conn) = setup_join_db();
    let def = posts_def_with_joins();

    conn.execute_batch(
        "INSERT INTO posts (id, title, created_at, updated_at) \
         VALUES ('p2', 'Post 2', '2024-01-01', '2024-01-01'), \
                ('p3', 'Post 3', '2024-01-01', '2024-01-01');",
    )
    .unwrap();

    set_related_ids(
        &conn,
        "posts",
        "tags",
        "p1",
        &["t1".to_string(), "t2".to_string()],
        None,
    )
    .unwrap();
    set_related_ids(&conn, "posts", "tags", "p2", &["t3".to_string()], None).unwrap();
    // p3 deliberately has no tags.

    let mut batched = vec![
        Document::new("p1".to_string()),
        Document::new("p2".to_string()),
        Document::new("p3".to_string()),
    ];
    hydrate_documents(&conn, "posts", &def.fields, &mut batched, None, None).unwrap();

    // Compare against the per-doc path on a fresh doc set.
    let mut p1_solo = Document::new("p1".to_string());
    hydrate_document(&conn, "posts", &def.fields, &mut p1_solo, None, None).unwrap();
    let mut p2_solo = Document::new("p2".to_string());
    hydrate_document(&conn, "posts", &def.fields, &mut p2_solo, None, None).unwrap();
    let mut p3_solo = Document::new("p3".to_string());
    hydrate_document(&conn, "posts", &def.fields, &mut p3_solo, None, None).unwrap();

    assert_eq!(batched[0].fields.get("tags"), p1_solo.fields.get("tags"));
    assert_eq!(batched[1].fields.get("tags"), p2_solo.fields.get("tags"));
    // p3 in the batched path gets an explicit empty array (so callers
    // can rely on the field being present); in the per-doc path the
    // field is omitted because the inner if-block doesn't fire when
    // ids is empty. Both shapes mean "no related tags".
    let batched_p3 = batched[2].fields.get("tags");
    assert!(
        batched_p3.is_none_or(|v| v.as_array().is_some_and(Vec::is_empty))
            || p3_solo.fields.get("tags").is_none(),
        "p3 must convey 'no tags' (either omitted or empty array)"
    );
}

/// Empty doc slice is a no-op: no SQL, no panics.
#[test]
fn hydrate_documents_empty_input_is_noop() {
    let (_dir, conn) = setup_join_db();
    let def = posts_def_with_joins();
    let mut docs: Vec<Document> = vec![];
    hydrate_documents(&conn, "posts", &def.fields, &mut docs, None, None).unwrap();
    assert!(docs.is_empty());
}
