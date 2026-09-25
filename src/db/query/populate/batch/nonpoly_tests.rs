//! Batch population of non-polymorphic relationships.

use serde_json::json;

use crate::core::cache::{CacheBackend, MemoryCache, NoneCache};
use crate::core::field::*;
use crate::core::{Document, Registry};
use crate::db::query::populate::batch::populate_relationships_batch_cached;
use crate::db::query::populate::test_helpers::*;
use crate::db::query::populate::{PopulateContext, PopulateOpts, populate_cache_key};
use rusqlite::Connection;

// ── Non-polymorphic has-one: shared refs ──────────────────────────────────

#[test]
fn batch_shared_has_one_refs() {
    // 3 posts all referencing the same author — batch should fetch author once
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, author TEXT, created_at TEXT, updated_at TEXT);
         CREATE TABLE authors (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);
         INSERT INTO authors VALUES ('a1', 'Alice', '2024-01-01', '2024-01-01');
         INSERT INTO authors VALUES ('a2', 'Bob', '2024-01-01', '2024-01-01');
         INSERT INTO posts VALUES ('p1', 'Post 1', 'a1', '2024-01-01', '2024-01-01');
         INSERT INTO posts VALUES ('p2', 'Post 2', 'a1', '2024-01-01', '2024-01-01');
         INSERT INTO posts VALUES ('p3', 'Post 3', 'a2', '2024-01-01', '2024-01-01');"
    ).unwrap();

    let registry = make_registry_with_posts_and_authors();
    let posts_def = make_posts_def();

    let mut docs = vec![
        {
            let mut d = Document::new("p1".to_string());
            d.fields.insert("title".to_string(), json!("Post 1"));
            d.fields.insert("author".to_string(), json!("a1"));
            d
        },
        {
            let mut d = Document::new("p2".to_string());
            d.fields.insert("title".to_string(), json!("Post 2"));
            d.fields.insert("author".to_string(), json!("a1"));
            d
        },
        {
            let mut d = Document::new("p3".to_string());
            d.fields.insert("title".to_string(), json!("Post 3"));
            d.fields.insert("author".to_string(), json!("a2"));
            d
        },
    ];

    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            fields: &posts_def.fields,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            published_only: false,
            join_access: None,
            user: None,
        },
        &NoneCache,
    )
    .unwrap();

    // All three should have populated authors
    for (i, doc) in docs.iter().enumerate() {
        let author = doc
            .fields
            .get("author")
            .unwrap_or_else(|| panic!("doc {i} missing author"));
        assert!(
            author.is_object(),
            "doc {i} author should be object, got {author:?}"
        );
    }
    // p1 and p2 share the same author
    assert_eq!(
        docs[0].fields["author"].get("id").unwrap().as_str(),
        Some("a1")
    );
    assert_eq!(
        docs[0].fields["author"].get("name").unwrap().as_str(),
        Some("Alice")
    );
    assert_eq!(
        docs[1].fields["author"].get("id").unwrap().as_str(),
        Some("a1")
    );
    assert_eq!(
        docs[2].fields["author"].get("id").unwrap().as_str(),
        Some("a2")
    );
    assert_eq!(
        docs[2].fields["author"].get("name").unwrap().as_str(),
        Some("Bob")
    );
}

// ── Non-polymorphic has-many ───────────────────────────────────────────────

#[test]
fn batch_has_many_fields() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE categories (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);
         CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, created_at TEXT, updated_at TEXT);
         INSERT INTO categories VALUES ('c1', 'Tech', '2024-01-01', '2024-01-01');
         INSERT INTO categories VALUES ('c2', 'Science', '2024-01-01', '2024-01-01');
         INSERT INTO categories VALUES ('c3', 'Art', '2024-01-01', '2024-01-01');
         INSERT INTO posts VALUES ('p1', 'Post 1', '2024-01-01', '2024-01-01');
         INSERT INTO posts VALUES ('p2', 'Post 2', '2024-01-01', '2024-01-01');"
    ).unwrap();

    let cats_def = make_collection_def("categories", vec![make_field("name", FieldType::Text)]);
    let mut tags_field = make_field("tags", FieldType::Relationship);
    tags_field.relationship = Some(RelationshipConfig::new("categories", true));
    let posts_def = make_collection_def(
        "posts",
        vec![make_field("title", FieldType::Text), tags_field],
    );

    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(cats_def);

    let mut docs = vec![
        {
            let mut d = Document::new("p1".to_string());
            d.fields.insert("tags".to_string(), json!(["c1", "c2"]));
            d
        },
        {
            let mut d = Document::new("p2".to_string());
            d.fields.insert("tags".to_string(), json!(["c2", "c3"]));
            d
        },
    ];

    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            fields: &posts_def.fields,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            published_only: false,
            join_access: None,
            user: None,
        },
        &NoneCache,
    )
    .unwrap();

    // p1 tags: Tech, Science
    let tags0 = docs[0].fields["tags"].as_array().unwrap();
    assert_eq!(tags0.len(), 2);
    assert_eq!(tags0[0].get("name").unwrap().as_str(), Some("Tech"));
    assert_eq!(tags0[1].get("name").unwrap().as_str(), Some("Science"));

    // p2 tags: Science, Art
    let tags1 = docs[1].fields["tags"].as_array().unwrap();
    assert_eq!(tags1.len(), 2);
    assert_eq!(tags1[0].get("name").unwrap().as_str(), Some("Science"));
    assert_eq!(tags1[1].get("name").unwrap().as_str(), Some("Art"));
}

// ── Non-poly has-one: cache hit in batch ──────────────────────────────────

#[test]
fn batch_has_one_cache_hit() {
    let conn = setup_populate_db();
    let registry = make_registry_with_posts_and_authors();
    let posts_def = make_posts_def();

    // Pre-populate cache with a different name to distinguish from DB
    let cache = MemoryCache::new(10_000);
    let mut cached_author = Document::new("a1".to_string());
    cached_author
        .fields
        .insert("name".to_string(), json!("CachedBatchAuthor"));
    let key = populate_cache_key("authors", "a1", None);
    cache
        .set(&key, &serde_json::to_vec(&cached_author).unwrap())
        .unwrap();

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
            fields: &posts_def.fields,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            published_only: false,
            join_access: None,
            user: None,
        },
        &cache,
    )
    .unwrap();

    let author = docs[0].fields.get("author").expect("author should exist");
    assert!(author.is_object(), "should be populated from cache");
    assert_eq!(
        author.get("name").and_then(|v| v.as_str()),
        Some("CachedBatchAuthor"),
        "batch has-one should use cache"
    );
}

// ── Non-poly has-many: cache hit in batch ─────────────────────────────────

#[test]
fn batch_has_many_cache_hit() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE categories (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);
         CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, created_at TEXT, updated_at TEXT);
         INSERT INTO categories VALUES ('c1', 'DBTech', '2024-01-01', '2024-01-01');
         INSERT INTO posts VALUES ('p1', 'Post', '2024-01-01', '2024-01-01');"
    ).unwrap();

    let cats_def = make_collection_def("categories", vec![make_field("name", FieldType::Text)]);
    let mut tags_field = make_field("tags", FieldType::Relationship);
    tags_field.relationship = Some(RelationshipConfig::new("categories", true));
    let posts_def = make_collection_def(
        "posts",
        vec![make_field("title", FieldType::Text), tags_field],
    );

    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(cats_def);

    // Pre-populate cache
    let cache = MemoryCache::new(10_000);
    let mut cached_cat = Document::new("c1".to_string());
    cached_cat
        .fields
        .insert("name".to_string(), json!("CachedCategory"));
    let key = populate_cache_key("categories", "c1", None);
    cache
        .set(&key, &serde_json::to_vec(&cached_cat).unwrap())
        .unwrap();

    let mut docs = vec![{
        let mut d = Document::new("p1".to_string());
        d.fields.insert("tags".to_string(), json!(["c1"]));
        d
    }];

    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            fields: &posts_def.fields,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            published_only: false,
            join_access: None,
            user: None,
        },
        &cache,
    )
    .unwrap();

    let tags = docs[0].fields.get("tags").expect("tags should exist");
    let arr = tags.as_array().expect("tags should be array");
    assert_eq!(arr.len(), 1);
    assert!(arr[0].is_object());
    assert_eq!(
        arr[0].get("name").and_then(|v| v.as_str()),
        Some("CachedCategory"),
        "batch has-many should use cache"
    );
}

// ── Unknown collection skips ──────────────────────────────────────────────

#[test]
fn batch_has_one_unknown_collection_skips() {
    let conn = setup_populate_db();

    let mut author_field = make_field("author", FieldType::Relationship);
    author_field.relationship = Some(RelationshipConfig::new("unknown_collection", false));
    let posts_def = make_collection_def(
        "posts",
        vec![make_field("title", FieldType::Text), author_field],
    );
    // Don't register "unknown_collection"
    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());

    let mut docs = vec![{
        let mut d = Document::new("p1".to_string());
        d.fields.insert("author".to_string(), json!("a1"));
        d
    }];

    // Should not panic; unknown collection is skipped via `continue`
    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            fields: &posts_def.fields,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            published_only: false,
            join_access: None,
            user: None,
        },
        &NoneCache,
    )
    .unwrap();

    // Field unchanged — unknown collection causes `continue`
    assert_eq!(
        docs[0].fields.get("author").and_then(|v| v.as_str()),
        Some("a1")
    );
}

// ── Regression: missing targets are dropped (has-many) / nulled (has-one) ──

#[test]
fn batch_has_many_missing_related_dropped() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE categories (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);
         CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, created_at TEXT, updated_at TEXT);
         INSERT INTO categories VALUES ('c1', 'Tech', '2024-01-01', '2024-01-01');
         INSERT INTO posts VALUES ('p1', 'Post 1', '2024-01-01', '2024-01-01');"
    ).unwrap();

    let cats_def = make_collection_def("categories", vec![make_field("name", FieldType::Text)]);
    let mut tags_field = make_field("tags", FieldType::Relationship);
    tags_field.relationship = Some(RelationshipConfig::new("categories", true));
    let posts_def = make_collection_def(
        "posts",
        vec![make_field("title", FieldType::Text), tags_field],
    );

    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(cats_def);

    let mut docs = vec![{
        let mut d = Document::new("p1".to_string());
        // Mix: existing + missing — missing should be dropped.
        d.fields
            .insert("tags".to_string(), json!(["missing1", "c1", "missing2"]));
        d
    }];

    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            fields: &posts_def.fields,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            published_only: false,
            join_access: None,
            user: None,
        },
        &NoneCache,
    )
    .unwrap();

    let tags = docs[0].fields.get("tags").expect("tags should exist");
    let arr = tags.as_array().expect("tags should be array");
    assert_eq!(arr.len(), 1, "missing has-many targets should be dropped");
    assert!(arr[0].is_object(), "remaining tag should be populated");
    assert_eq!(arr[0].get("id").and_then(|v| v.as_str()), Some("c1"));
}

#[test]
fn batch_has_many_soft_deleted_target_dropped() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE categories (id TEXT PRIMARY KEY, name TEXT, _deleted_at TEXT, created_at TEXT, updated_at TEXT);
         CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, created_at TEXT, updated_at TEXT);
         INSERT INTO categories (id, name, _deleted_at, created_at, updated_at)
            VALUES ('c1', 'Tech', NULL, '2024-01-01', '2024-01-01');
         INSERT INTO categories (id, name, _deleted_at, created_at, updated_at)
            VALUES ('c2', 'Science', '2024-02-01', '2024-01-01', '2024-01-01');
         INSERT INTO posts VALUES ('p1', 'Post 1', '2024-01-01', '2024-01-01');"
    ).unwrap();

    let mut cats_def = make_collection_def("categories", vec![make_field("name", FieldType::Text)]);
    cats_def.soft_delete = true;

    let mut tags_field = make_field("tags", FieldType::Relationship);
    tags_field.relationship = Some(RelationshipConfig::new("categories", true));
    let posts_def = make_collection_def(
        "posts",
        vec![make_field("title", FieldType::Text), tags_field],
    );

    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(cats_def);

    let mut docs = vec![{
        let mut d = Document::new("p1".to_string());
        d.fields.insert("tags".to_string(), json!(["c1", "c2"]));
        d
    }];

    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            fields: &posts_def.fields,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            published_only: false,
            join_access: None,
            user: None,
        },
        &NoneCache,
    )
    .unwrap();

    let tags = docs[0].fields.get("tags").expect("tags should exist");
    let arr = tags.as_array().expect("tags should be array");
    assert_eq!(arr.len(), 1, "soft-deleted target should be dropped");
    assert_eq!(arr[0].get("name").and_then(|v| v.as_str()), Some("Tech"));
}

#[test]
fn batch_has_one_soft_deleted_target_null() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE authors (id TEXT PRIMARY KEY, name TEXT, _deleted_at TEXT, created_at TEXT, updated_at TEXT);
         CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, author TEXT, created_at TEXT, updated_at TEXT);
         INSERT INTO authors (id, name, _deleted_at, created_at, updated_at)
            VALUES ('a1', 'Alice', '2024-02-01', '2024-01-01', '2024-01-01');
         INSERT INTO posts VALUES ('p1', 'Post', 'a1', '2024-01-01', '2024-01-01');"
    ).unwrap();

    let mut authors_def = make_collection_def("authors", vec![make_field("name", FieldType::Text)]);
    authors_def.soft_delete = true;

    let mut author_field = make_field("author", FieldType::Relationship);
    author_field.relationship = Some(RelationshipConfig::new("authors", false));
    let posts_def = make_collection_def(
        "posts",
        vec![make_field("title", FieldType::Text), author_field],
    );

    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(authors_def);

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
            fields: &posts_def.fields,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            published_only: false,
            join_access: None,
            user: None,
        },
        &NoneCache,
    )
    .unwrap();

    assert_eq!(
        docs[0].fields.get("author"),
        Some(&serde_json::Value::Null),
        "soft-deleted has-one target should be null"
    );
}

// ── Regression: visited cycle protection still keeps as ID string ─────────

#[test]
fn batch_has_many_visited_kept_as_string() {
    // Self-referential: a category tagged with itself. The dispatcher
    // pre-seeds visited with (ctx.collection_slug, doc.id), so
    // ("categories", "c1") is visited — the tag "c1" must stay as a raw
    // string (NOT dropped, NOT populated).
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE categories (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);
         INSERT INTO categories VALUES ('c1', 'Tech', '2024-01-01', '2024-01-01');",
    )
    .unwrap();

    let mut tags_field = make_field("tags", FieldType::Relationship);
    tags_field.relationship = Some(RelationshipConfig::new("categories", true));
    let cats_def = make_collection_def(
        "categories",
        vec![make_field("name", FieldType::Text), tags_field],
    );

    let mut registry = Registry::new();
    registry.register_collection(cats_def.clone());

    let mut docs = vec![{
        let mut d = Document::new("c1".to_string());
        d.fields.insert("tags".to_string(), json!(["c1"]));
        d
    }];

    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "categories",
            fields: &cats_def.fields,
        },
        &mut docs,
        &PopulateOpts {
            depth: 2,
            select: None,
            locale_ctx: None,
            published_only: false,
            join_access: None,
            user: None,
        },
        &NoneCache,
    )
    .unwrap();

    let tags = docs[0].fields.get("tags").expect("tags should exist");
    let arr = tags.as_array().expect("tags should be array");
    assert_eq!(arr.len(), 1);
    assert_eq!(
        arr[0].as_str(),
        Some("c1"),
        "visited cycle ref should stay as string, not be dropped"
    );
}

#[test]
fn batch_has_many_unknown_collection_skips() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, created_at TEXT, updated_at TEXT);
         INSERT INTO posts VALUES ('p1', 'Post', '2024-01-01', '2024-01-01');",
    )
    .unwrap();

    let mut tags_field = make_field("tags", FieldType::Relationship);
    tags_field.relationship = Some(RelationshipConfig::new("unknown_collection", true));
    let posts_def = make_collection_def(
        "posts",
        vec![make_field("title", FieldType::Text), tags_field],
    );
    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());

    let mut docs = vec![{
        let mut d = Document::new("p1".to_string());
        d.fields.insert("tags".to_string(), json!(["t1"]));
        d
    }];

    // Should not panic; unknown collection causes `continue`
    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            fields: &posts_def.fields,
        },
        &mut docs,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            published_only: false,
            join_access: None,
            user: None,
        },
        &NoneCache,
    )
    .unwrap();

    // tags field unchanged
    let tags = docs[0].fields.get("tags").expect("tags should exist");
    assert!(tags.as_array().is_some());
}
