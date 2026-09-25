//! Batch population of polymorphic relationships.

use serde_json::json;

use crate::core::cache::{CacheBackend, MemoryCache, NoneCache};
use crate::core::field::*;
use crate::core::{Document, Registry};
use crate::db::{
    DbConnection,
    query::{
        PopulateContext, PopulateOpts, join,
        populate::{populate_cache_key, populate_relationships_batch_cached, test_helpers::*},
    },
};

// ── Polymorphic has-one (batch) ────────────────────────────────────────────

#[test]
fn batch_polymorphic_has_one() {
    let conn = setup_polymorphic_populate_db();
    let entries_def = make_entries_def_poly_has_one();
    let articles_def = make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
    let pages_def = make_collection_def("pages", vec![make_field("title", FieldType::Text)]);
    let mut registry = Registry::new();
    registry.register_collection(entries_def.clone());
    registry.register_collection(articles_def);
    registry.register_collection(pages_def);

    let mut docs = vec![{
        let mut d = Document::new("e1".to_string());
        d.fields.insert("title".to_string(), json!("Entry"));
        d.fields.insert("related".to_string(), json!("articles/a1"));
        d
    }];

    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "entries",
            fields: &entries_def.fields,
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

    let related = &docs[0].fields["related"];
    assert!(
        related.is_object(),
        "polymorphic has-one should be populated"
    );
    assert_eq!(related.get("id").unwrap().as_str(), Some("a1"));
    assert_eq!(
        related.get("collection").unwrap().as_str(),
        Some("articles")
    );
}

// ── Polymorphic has-many (batch) ──────────────────────────────────────────

#[test]
fn batch_polymorphic_has_many() {
    let conn = setup_polymorphic_populate_db();
    conn.execute_batch(
        "INSERT INTO entries_refs (parent_id, related_id, related_collection, _order)
            VALUES ('e1', 'a1', 'articles', 0), ('e1', 'pg1', 'pages', 1);",
    )
    .unwrap();

    let entries_def = make_entries_def_poly_has_many();
    let articles_def = make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
    let pages_def = make_collection_def("pages", vec![make_field("title", FieldType::Text)]);
    let mut registry = Registry::new();
    registry.register_collection(entries_def.clone());
    registry.register_collection(articles_def);
    registry.register_collection(pages_def);

    let mut doc = Document::new("e1".to_string());
    doc.fields.insert("title".to_string(), json!("Entry"));
    join::hydrate_document(&conn, "entries", &entries_def.fields, &mut doc, None, None).unwrap();

    let mut docs = vec![doc];
    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "entries",
            fields: &entries_def.fields,
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

    let refs = docs[0].fields.get("refs").unwrap();
    let arr = refs.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert!(arr[0].is_object());
    assert_eq!(arr[0].get("collection").unwrap().as_str(), Some("articles"));
    assert!(arr[1].is_object());
    assert_eq!(arr[1].get("collection").unwrap().as_str(), Some("pages"));
}

// ── Polymorphic has-many: unknown collection in distribution ──────────────

#[test]
fn batch_polymorphic_has_many_unknown_col_in_distribution_keeps_string() {
    let conn = setup_polymorphic_populate_db();
    let entries_def = make_entries_def_poly_has_many();
    // Only register "articles", not "videos" which will be in the data
    let articles_def = make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
    let mut registry = Registry::new();
    registry.register_collection(entries_def.clone());
    registry.register_collection(articles_def);

    let mut doc = Document::new("e1".to_string());
    // Mix: one with known collection, one with unknown collection
    doc.fields
        .insert("refs".to_string(), json!(["articles/a1", "videos/v1"]));

    let mut docs = vec![doc];
    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "entries",
            fields: &entries_def.fields,
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

    let refs = docs[0].fields.get("refs").expect("refs should exist");
    let arr = refs.as_array().expect("refs should be array");
    assert_eq!(arr.len(), 2);
    // Known collection: populated
    assert!(arr[0].is_object(), "known collection should be populated");
    // Unknown collection: fetched_map won't contain "videos", stays as string
    assert_eq!(
        arr[1].as_str(),
        Some("videos/v1"),
        "unknown collection in batch poly has-many should remain as string"
    );
}

// ── Polymorphic has-many: malformed item ──────────────────────────────────

#[test]
fn batch_polymorphic_has_many_malformed_item_keeps_string() {
    let conn = setup_polymorphic_populate_db();
    let entries_def = make_entries_def_poly_has_many();
    let articles_def = make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
    let mut registry = Registry::new();
    registry.register_collection(entries_def.clone());
    registry.register_collection(articles_def);

    let mut doc = Document::new("e1".to_string());
    // Mix: valid composite string, and a malformed one (no slash)
    doc.fields
        .insert("refs".to_string(), json!(["articles/a1", "badformat"]));

    let mut docs = vec![doc];
    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "entries",
            fields: &entries_def.fields,
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

    let refs = docs[0].fields.get("refs").expect("refs should exist");
    let arr = refs.as_array().expect("refs should be array");
    assert_eq!(arr.len(), 2);
    assert!(arr[0].is_object(), "valid poly ref should be populated");
    assert_eq!(
        arr[1].as_str(),
        Some("badformat"),
        "malformed poly ref should remain as string in batch"
    );
}

// ── Polymorphic has-many: doc not in fetched col_map ─────────────────────

#[test]
fn batch_polymorphic_has_many_missing_doc_is_dropped() {
    let conn = setup_polymorphic_populate_db();
    let entries_def = make_entries_def_poly_has_many();
    // Register "articles" in registry so fetched_map gets an entry, but fetch
    // "nonexistent" id which won't be in the returned results
    let articles_def = make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
    let mut registry = Registry::new();
    registry.register_collection(entries_def.clone());
    registry.register_collection(articles_def);

    let mut doc = Document::new("e1".to_string());
    // "articles" is a known collection, but "nope" doesn't exist in DB.
    // Mix with a valid ref to ensure only the missing one is dropped.
    doc.fields
        .insert("refs".to_string(), json!(["articles/a1", "articles/nope"]));

    let mut docs = vec![doc];
    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "entries",
            fields: &entries_def.fields,
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

    let refs = docs[0].fields.get("refs").expect("refs should exist");
    let arr = refs.as_array().expect("refs should be array");
    // Missing doc in a known collection is a DB miss → dropped from the array.
    assert_eq!(arr.len(), 1, "missing target should be dropped");
    assert!(arr[0].is_object(), "remaining ref should be populated");
    assert_eq!(arr[0].get("id").and_then(|v| v.as_str()), Some("a1"));
}

// ── Polymorphic has-one: unknown collection in distribution ───────────────

#[test]
fn batch_polymorphic_has_one_unknown_col_in_distribution_keeps_string() {
    let conn = setup_polymorphic_populate_db();
    let entries_def = make_entries_def_poly_has_one();
    // Don't register "videos" in registry
    let articles_def = make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
    let mut registry = Registry::new();
    registry.register_collection(entries_def.clone());
    registry.register_collection(articles_def);

    let mut doc = Document::new("e1".to_string());
    // "videos" collection is not registered
    doc.fields.insert("related".to_string(), json!("videos/v1"));

    let mut docs = vec![doc];
    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "entries",
            fields: &entries_def.fields,
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

    // "videos" not in fetched_map → value unchanged
    assert_eq!(
        docs[0].fields.get("related").and_then(|v| v.as_str()),
        Some("videos/v1"),
        "unknown collection in batch poly has-one distribution should remain as string"
    );
}

// ── Polymorphic has-one: visited is skipped ───────────────────────────────

#[test]
fn batch_polymorphic_has_one_visited_is_skipped() {
    let conn = setup_polymorphic_populate_db();
    let entries_def = make_entries_def_poly_has_one();
    let articles_def = make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
    let mut registry = Registry::new();
    registry.register_collection(entries_def.clone());
    registry.register_collection(articles_def);

    let mut doc = Document::new("e1".to_string());
    doc.fields
        .insert("related".to_string(), json!("articles/a1"));

    let cache = MemoryCache::new(10_000);
    let mut docs = vec![doc];

    // Pre-populate cache so the doc is returned from cache in distribution
    let mut cached_article = Document::new("a1".to_string());
    cached_article
        .fields
        .insert("title".to_string(), json!("CachedFromBatchCache"));
    let key = populate_cache_key("articles", "a1", None);
    cache
        .set(&key, &serde_json::to_vec(&cached_article).unwrap())
        .unwrap();

    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "entries",
            fields: &entries_def.fields,
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

    let related = docs[0].fields.get("related").expect("related should exist");
    assert!(related.is_object(), "should be populated");
    assert_eq!(
        related.get("title").and_then(|v| v.as_str()),
        Some("CachedFromBatchCache"),
        "should use cached document in batch poly has-one"
    );
}

// ── Regression: soft-deleted poly targets dropped (has-many) / nulled (has-one) ──

#[test]
fn batch_polymorphic_has_many_soft_deleted_target_dropped() {
    let conn = setup_polymorphic_populate_db();
    conn.execute_batch(
        "ALTER TABLE articles ADD COLUMN _deleted_at TEXT;
         UPDATE articles SET _deleted_at = '2024-02-01' WHERE id = 'a1';
         INSERT INTO entries_refs (parent_id, related_id, related_collection, _order)
            VALUES ('e1', 'a1', 'articles', 0), ('e1', 'pg1', 'pages', 1);",
    )
    .unwrap();

    let entries_def = make_entries_def_poly_has_many();
    let mut articles_def =
        make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
    articles_def.soft_delete = true;
    let pages_def = make_collection_def("pages", vec![make_field("title", FieldType::Text)]);

    let mut registry = Registry::new();
    registry.register_collection(entries_def.clone());
    registry.register_collection(articles_def);
    registry.register_collection(pages_def);

    let mut doc = Document::new("e1".to_string());
    doc.fields.insert("title".to_string(), json!("Entry"));
    join::hydrate_document(&conn, "entries", &entries_def.fields, &mut doc, None, None).unwrap();

    let mut docs = vec![doc];
    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "entries",
            fields: &entries_def.fields,
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

    let refs = docs[0].fields.get("refs").expect("refs should exist");
    let arr = refs.as_array().expect("refs should be array");
    assert_eq!(arr.len(), 1, "soft-deleted poly target should be dropped");
    assert_eq!(arr[0].get("id").and_then(|v| v.as_str()), Some("pg1"));
}

#[test]
fn batch_polymorphic_has_one_missing_target_null() {
    let conn = setup_polymorphic_populate_db();
    let entries_def = make_entries_def_poly_has_one();
    let articles_def = make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
    let mut registry = Registry::new();
    registry.register_collection(entries_def.clone());
    registry.register_collection(articles_def);

    let mut doc = Document::new("e1".to_string());
    // "articles" is known, but "nope" doesn't exist → DB miss.
    doc.fields
        .insert("related".to_string(), json!("articles/nope"));

    let mut docs = vec![doc];
    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "entries",
            fields: &entries_def.fields,
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
        docs[0].fields.get("related"),
        Some(&serde_json::Value::Null),
        "missing poly has-one target should be null"
    );
}

#[test]
fn batch_polymorphic_has_one_soft_deleted_target_null() {
    let conn = setup_polymorphic_populate_db();
    conn.execute_batch(
        "ALTER TABLE articles ADD COLUMN _deleted_at TEXT;
         UPDATE articles SET _deleted_at = '2024-02-01' WHERE id = 'a1';",
    )
    .unwrap();

    let entries_def = make_entries_def_poly_has_one();
    let mut articles_def =
        make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
    articles_def.soft_delete = true;
    let mut registry = Registry::new();
    registry.register_collection(entries_def.clone());
    registry.register_collection(articles_def);

    let mut doc = Document::new("e1".to_string());
    doc.fields
        .insert("related".to_string(), json!("articles/a1"));

    let mut docs = vec![doc];
    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "entries",
            fields: &entries_def.fields,
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
        docs[0].fields.get("related"),
        Some(&serde_json::Value::Null),
        "soft-deleted poly has-one target should be null"
    );
}

// ── Polymorphic has-one: cache hit in batch ───────────────────────────────

#[test]
fn batch_polymorphic_has_one_cache_hit() {
    let conn = setup_polymorphic_populate_db();
    let entries_def = make_entries_def_poly_has_one();
    let articles_def = make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
    let mut registry = Registry::new();
    registry.register_collection(entries_def.clone());
    registry.register_collection(articles_def);

    // Pre-populate cache so find_by_ids is skipped for this id
    let cache = MemoryCache::new(10_000);
    let mut cached_article = Document::new("a1".to_string());
    cached_article
        .fields
        .insert("title".to_string(), json!("CachedTitle"));
    let key = populate_cache_key("articles", "a1", None);
    cache
        .set(&key, &serde_json::to_vec(&cached_article).unwrap())
        .unwrap();

    let mut doc = Document::new("e1".to_string());
    doc.fields
        .insert("related".to_string(), json!("articles/a1"));
    let mut docs = vec![doc];

    populate_relationships_batch_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "entries",
            fields: &entries_def.fields,
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

    let related = docs[0].fields.get("related").expect("related should exist");
    assert!(related.is_object());
    assert_eq!(
        related.get("title").and_then(|v| v.as_str()),
        Some("CachedTitle"),
        "batch poly has-one should use cache"
    );
}
