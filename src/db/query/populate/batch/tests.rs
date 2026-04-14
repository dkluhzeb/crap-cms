use rusqlite::Connection;
use serde_json::json;

use crate::core::cache::NoneCache;
use crate::core::{
    Document, Registry,
    field::{FieldType, RelationshipConfig},
};
use crate::db::query::populate::{
    PopulateContext, PopulateOpts, populate_relationships_batch,
    populate_relationships_batch_cached, test_helpers::*,
};

// ── Basic depth/empty guard ───────────────────────────────────────────────

#[test]
fn batch_depth_zero_noop() {
    let conn = setup_populate_db();
    let registry = make_registry_with_posts_and_authors();
    let posts_def = make_posts_def();

    let mut docs = vec![];
    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut docs,
        &PopulateOpts {
            depth: 0,
            select: None,
            locale_ctx: None,
            join_access: None,
            user: None,
        },
        &NoneCache,
    )
    .unwrap();
    // Empty docs + depth 0 → no-op, no error
}

#[test]
fn batch_empty_docs_noop() {
    let conn = setup_populate_db();
    let registry = make_registry_with_posts_and_authors();
    let posts_def = make_posts_def();

    let mut docs = vec![];
    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            join_access: None,
            user: None,
        },
        &NoneCache,
    )
    .unwrap();
}

// ── Select filtering ──────────────────────────────────────────────────────

#[test]
fn batch_select_filters_fields() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, author TEXT, editor TEXT, created_at TEXT, updated_at TEXT);
         CREATE TABLE authors (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);
         INSERT INTO authors VALUES ('a1', 'Alice', '2024-01-01', '2024-01-01');
         INSERT INTO posts VALUES ('p1', 'Post 1', 'a1', 'a1', '2024-01-01', '2024-01-01');"
    ).unwrap();

    let mut author_field = make_field("author", FieldType::Relationship);
    author_field.relationship = Some(RelationshipConfig::new("authors", false));
    let mut editor_field = make_field("editor", FieldType::Relationship);
    editor_field.relationship = Some(RelationshipConfig::new("authors", false));
    let posts_def = make_collection_def(
        "posts",
        vec![
            make_field("title", FieldType::Text),
            author_field,
            editor_field,
        ],
    );
    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(make_authors_def());

    let mut docs = vec![{
        let mut d = Document::new("p1".to_string());
        d.fields.insert("author".to_string(), json!("a1"));
        d.fields.insert("editor".to_string(), json!("a1"));
        d
    }];

    let select = vec!["author".to_string()];
    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: Some(&select),
            locale_ctx: None,
            join_access: None,
            user: None,
        },
        &NoneCache,
    )
    .unwrap();

    // author should be populated
    assert!(docs[0].fields["author"].is_object());
    // editor should remain as ID (not in select)
    assert_eq!(docs[0].fields["editor"].as_str(), Some("a1"));
}

// ── Field-level max_depth ─────────────────────────────────────────────────

#[test]
fn batch_max_depth_zero_stays_as_id() {
    let conn = setup_populate_db();

    let mut author_field = make_field("author", FieldType::Relationship);
    let mut rel = RelationshipConfig::new("authors", false);
    rel.max_depth = Some(0);
    author_field.relationship = Some(rel);
    let posts_def = make_collection_def(
        "posts",
        vec![make_field("title", FieldType::Text), author_field],
    );
    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(make_authors_def());

    let mut docs = vec![{
        let mut d = Document::new("p1".to_string());
        d.fields.insert("author".to_string(), json!("a1"));
        d
    }];

    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            join_access: None,
            user: None,
        },
        &NoneCache,
    )
    .unwrap();

    // max_depth=0 should prevent population
    assert_eq!(docs[0].fields["author"].as_str(), Some("a1"));
}

// ── Missing related docs ──────────────────────────────────────────────────

#[test]
fn batch_missing_related_has_one_becomes_null() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, author TEXT, created_at TEXT, updated_at TEXT);
         CREATE TABLE authors (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);
         INSERT INTO posts VALUES ('p1', 'Post 1', 'nonexistent', '2024-01-01', '2024-01-01');"
    ).unwrap();

    let registry = make_registry_with_posts_and_authors();
    let posts_def = make_posts_def();

    let mut docs = vec![{
        let mut d = Document::new("p1".to_string());
        d.fields.insert("author".to_string(), json!("nonexistent"));
        d
    }];

    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            join_access: None,
            user: None,
        },
        &NoneCache,
    )
    .unwrap();

    // Missing has-one target is set to null (not kept as raw ID).
    assert_eq!(docs[0].fields.get("author"), Some(&serde_json::Value::Null));
}

// ── Join fields in batch ──────────────────────────────────────────────────

#[test]
fn batch_with_join_field() {
    let conn = setup_join_db();
    let authors_def = make_authors_def_with_join();
    let posts_def = make_posts_def_for_join();
    let mut registry = Registry::new();
    registry.register_collection(authors_def.clone());
    registry.register_collection(posts_def);

    let mut docs = vec![{
        let mut d = Document::new("a1".to_string());
        d.fields.insert("name".to_string(), json!("Alice"));
        d
    }];

    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "authors",
            def: &authors_def,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            join_access: None,
            user: None,
        },
        &NoneCache,
    )
    .unwrap();

    let posts = docs[0]
        .fields
        .get("posts")
        .expect("join field should be populated");
    let arr = posts.as_array().unwrap();
    assert_eq!(arr.len(), 2, "Alice has 2 posts");
}

// ── populate_relationships_batch wrapper ──────────────────────────────────

#[test]
fn populate_relationships_batch_wrapper_creates_fresh_cache() {
    let conn = setup_populate_db();
    let registry = make_registry_with_posts_and_authors();
    let posts_def = make_posts_def();

    let mut docs = vec![{
        let mut d = Document::new("p1".to_string());
        d.fields.insert("author".to_string(), json!("a1"));
        d
    }];

    // wrapper should succeed (creates fresh cache internally)
    populate_relationships_batch(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            join_access: None,
            user: None,
        },
    )
    .unwrap();
    assert!(docs[0].fields["author"].is_object());
    assert_eq!(
        docs[0].fields["author"]
            .get("name")
            .and_then(|v| v.as_str()),
        Some("Alice")
    );
}
