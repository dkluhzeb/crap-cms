use std::collections::HashSet;

use rusqlite::Connection;
use serde_json::json;

use crate::core::cache::NoneCache;
use crate::core::{
    Document, FieldDefinition, Registry,
    field::{BlockDefinition, FieldTab, FieldType, RelationshipConfig},
};
use crate::db::DbConnection;
use crate::db::query::populate::{
    PopulateContext, PopulateOpts, populate_relationships, populate_relationships_cached,
    test_helpers::*,
};

// ── populate_relationships (depth / basic hydration) ──────────────────────

#[test]
fn populate_depth_zero_noop() {
    let conn = setup_populate_db();
    let registry = make_registry_with_posts_and_authors();
    let posts_def = make_posts_def();

    let mut doc = Document::new("p1".to_string());
    doc.fields.insert("title".to_string(), json!("Hello"));
    doc.fields.insert("author".to_string(), json!("a1"));
    doc.created_at = Some("2024-01-01".to_string());
    doc.updated_at = Some("2024-01-01".to_string());

    let mut visited = HashSet::new();
    populate_relationships_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut doc,
        &mut visited,
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

    // Author field should remain a string ID, not populated
    assert_eq!(
        doc.fields.get("author").and_then(|v| v.as_str()),
        Some("a1"),
        "depth=0 should not modify the document"
    );
}

#[test]
fn populate_depth_one_hydrates() {
    let conn = setup_populate_db();
    let registry = make_registry_with_posts_and_authors();
    let posts_def = make_posts_def();

    let mut doc = Document::new("p1".to_string());
    doc.fields.insert("title".to_string(), json!("Hello"));
    doc.fields.insert("author".to_string(), json!("a1"));
    doc.created_at = Some("2024-01-01".to_string());
    doc.updated_at = Some("2024-01-01".to_string());

    let mut visited = HashSet::new();
    populate_relationships_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut doc,
        &mut visited,
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

    // Author field should now be a populated object
    let author = doc.fields.get("author").expect("author field should exist");
    assert!(
        author.is_object(),
        "author should be populated as an object, got {:?}",
        author
    );

    let author_obj = author.as_object().unwrap();
    assert_eq!(author_obj.get("id").and_then(|v| v.as_str()), Some("a1"));
    assert_eq!(
        author_obj.get("name").and_then(|v| v.as_str()),
        Some("Alice")
    );
    assert_eq!(
        author_obj.get("collection").and_then(|v| v.as_str()),
        Some("authors")
    );
}

#[test]
fn populate_circular_ref_stops() {
    // Set up two collections that reference each other: posts -> authors, authors -> posts
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE posts (
            id TEXT PRIMARY KEY,
            title TEXT,
            author TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        CREATE TABLE authors (
            id TEXT PRIMARY KEY,
            name TEXT,
            favorite_post TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        INSERT INTO authors (id, name, favorite_post, created_at, updated_at)
            VALUES ('a1', 'Alice', 'p1', '2024-01-01', '2024-01-01');
        INSERT INTO posts (id, title, author, created_at, updated_at)
            VALUES ('p1', 'Hello', 'a1', '2024-01-01', '2024-01-01');",
    )
    .unwrap();

    // Authors def with a relationship back to posts
    let mut fav_post_field = make_field("favorite_post", FieldType::Relationship);
    fav_post_field.relationship = Some(RelationshipConfig::new("posts", false));
    let authors_def = make_collection_def(
        "authors",
        vec![make_field("name", FieldType::Text), fav_post_field],
    );

    // Posts def with relationship to authors
    let posts_def = make_posts_def();

    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(authors_def);

    let mut doc = Document::new("p1".to_string());
    doc.fields.insert("title".to_string(), json!("Hello"));
    doc.fields.insert("author".to_string(), json!("a1"));
    doc.created_at = Some("2024-01-01".to_string());
    doc.updated_at = Some("2024-01-01".to_string());

    // Pre-populate visited with the post itself to simulate already being in the chain
    let mut visited = HashSet::new();

    // Use high depth to ensure circular ref protection kicks in rather than depth limit
    let result = populate_relationships_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut doc,
        &mut visited,
        &PopulateOpts {
            depth: 10,
            select: None,
            locale_ctx: None,
            join_access: None,
            user: None,
        },
        &NoneCache,
    );

    assert!(
        result.is_ok(),
        "should not infinite loop on circular references"
    );

    // The author should be populated (first visit)
    let author = doc.fields.get("author").expect("author field should exist");
    assert!(author.is_object(), "author should be populated");

    // But the author's favorite_post should NOT be re-populated as a full object
    // because posts/p1 was already visited
    let author_obj = author.as_object().unwrap();
    let fav_post = author_obj.get("favorite_post");
    // It should either be the original string ID or absent (kept as-is due to visited check)
    if let Some(fp) = fav_post {
        assert!(
            fp.is_string(),
            "favorite_post should remain a string ID due to circular ref, got {:?}",
            fp
        );
    }
}

#[test]
fn populate_field_level_max_depth_caps() {
    let conn = setup_populate_db();

    // Create a field with max_depth = 0 — should not populate even at depth=1
    let mut author_field = make_field("author", FieldType::Relationship);
    author_field.relationship = Some({
        let mut r = RelationshipConfig::new("authors", false);
        r.max_depth = Some(0);
        r
    });
    let posts_def = make_collection_def(
        "posts",
        vec![make_field("title", FieldType::Text), author_field],
    );

    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(make_authors_def());

    let mut doc = Document::new("p1".to_string());
    doc.fields.insert("title".to_string(), json!("Hello"));
    doc.fields.insert("author".to_string(), json!("a1"));

    let mut visited = HashSet::new();
    populate_relationships_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut doc,
        &mut visited,
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

    // author should remain a string ID because max_depth=0 caps effective_depth to 0
    assert_eq!(
        doc.fields.get("author").and_then(|v| v.as_str()),
        Some("a1"),
        "field-level max_depth=0 should prevent population"
    );
}

#[test]
fn populate_select_filters_fields() {
    let conn = setup_populate_db();

    // Add a second relationship field
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

    let mut doc = Document::new("p1".to_string());
    doc.fields.insert("title".to_string(), json!("Hello"));
    doc.fields.insert("author".to_string(), json!("a1"));
    doc.fields.insert("editor".to_string(), json!("a1"));

    let mut visited = HashSet::new();
    let select = vec!["author".to_string()]; // Only populate author, not editor
    populate_relationships_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut doc,
        &mut visited,
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
    let author = doc.fields.get("author").expect("author should exist");
    assert!(author.is_object(), "author should be populated");

    // editor should remain a string ID (not in select)
    assert_eq!(
        doc.fields.get("editor").and_then(|v| v.as_str()),
        Some("a1"),
        "editor should not be populated when not in select"
    );
}

#[test]
fn populate_has_one_empty_string_skipped() {
    let conn = setup_populate_db();
    let registry = make_registry_with_posts_and_authors();
    let posts_def = make_posts_def();

    let mut doc = Document::new("p1".to_string());
    doc.fields.insert("title".to_string(), json!("Hello"));
    doc.fields.insert("author".to_string(), json!(""));

    let mut visited = HashSet::new();
    populate_relationships_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut doc,
        &mut visited,
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

    // Empty string ID should be skipped (the `_ => continue` branch)
    assert_eq!(
        doc.fields.get("author").and_then(|v| v.as_str()),
        Some(""),
        "empty string author should not be populated"
    );
}

#[test]
fn populate_relationships_wrapper_creates_fresh_cache() {
    // The wrapper creates a fresh NoneCache per call — verify it works
    // by calling it twice and confirming it doesn't error (each call is independent)
    let conn = setup_populate_db();
    let registry = make_registry_with_posts_and_authors();
    let posts_def = make_posts_def();

    let mut doc = Document::new("p1".to_string());
    doc.fields.insert("author".to_string(), json!("a1"));

    let mut visited = HashSet::new();
    // First call — populates
    populate_relationships(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut doc,
        &mut visited,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            join_access: None,
            user: None,
        },
    )
    .unwrap();
    assert!(doc.fields["author"].is_object());

    // Reset and call again to confirm fresh cache (no stale state between calls)
    let mut doc2 = Document::new("p1".to_string());
    doc2.fields.insert("author".to_string(), json!("a1"));
    let mut visited2 = HashSet::new();
    populate_relationships(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut doc2,
        &mut visited2,
        &PopulateOpts {
            depth: 1,
            select: None,
            locale_ctx: None,
            join_access: None,
            user: None,
        },
    )
    .unwrap();
    assert!(doc2.fields["author"].is_object());
    assert_eq!(
        doc2.fields["author"].get("name").and_then(|v| v.as_str()),
        Some("Alice")
    );
}

// ── nested container population tests ─────────────────────────────────

#[test]
fn populate_upload_inside_blocks() {
    let conn = setup_populate_db();
    // Add media table (simulating an upload collection)
    conn.execute_batch(
        "CREATE TABLE media (
            id TEXT PRIMARY KEY,
            filename TEXT,
            url TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        INSERT INTO media (id, filename, url, created_at, updated_at)
            VALUES ('m1', 'hero.jpg', '/uploads/hero.jpg', '2024-01-01', '2024-01-01');
        CREATE TABLE pages (
            id TEXT PRIMARY KEY,
            title TEXT,
            content TEXT,
            created_at TEXT,
            updated_at TEXT
        );
        INSERT INTO pages (id, title, content, created_at, updated_at)
            VALUES ('pg1', 'Home', '[]', '2024-01-01', '2024-01-01');",
    )
    .unwrap();

    let media_def = make_collection_def(
        "media",
        vec![
            make_field("filename", FieldType::Text),
            make_field("url", FieldType::Text),
        ],
    );

    // Build pages def with a blocks field containing a hero block with an upload sub-field
    let mut bg_field = make_field("background", FieldType::Upload);
    bg_field.relationship = Some(RelationshipConfig::new("media", false));
    let hero_block = BlockDefinition::new(
        "hero",
        vec![make_field("heading", FieldType::Text), bg_field],
    );
    let content_field = FieldDefinition::builder("content", FieldType::Blocks)
        .blocks(vec![hero_block])
        .build();
    let pages_def = make_collection_def(
        "pages",
        vec![make_field("title", FieldType::Text), content_field],
    );

    let mut registry = Registry::new();
    registry.register_collection(pages_def.clone());
    registry.register_collection(media_def);

    let mut doc = Document::new("pg1".to_string());
    doc.fields.insert("title".to_string(), json!("Home"));
    doc.fields.insert(
        "content".to_string(),
        json!([{
            "_block_type": "hero",
            "heading": "Welcome",
            "background": "m1"
        }]),
    );

    let mut visited = HashSet::new();
    populate_relationships_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "pages",
            def: &pages_def,
        },
        &mut doc,
        &mut visited,
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

    let content = doc.fields.get("content").expect("content should exist");
    let blocks = content.as_array().expect("content should be array");
    assert_eq!(blocks.len(), 1);

    let hero = blocks[0].as_object().expect("block should be object");
    let bg = hero.get("background").expect("background should exist");
    assert!(
        bg.is_object(),
        "upload inside block should be populated, got {:?}",
        bg
    );
    assert_eq!(bg.get("id").and_then(|v| v.as_str()), Some("m1"));
    assert_eq!(
        bg.get("filename").and_then(|v| v.as_str()),
        Some("hero.jpg")
    );
}

#[test]
fn populate_relationship_inside_tabs() {
    let conn = setup_populate_db();
    // posts and authors tables already exist from setup_populate_db

    // Build a collection with a tabs field containing a relationship sub-field
    let mut author_field = make_field("author", FieldType::Relationship);
    author_field.relationship = Some(RelationshipConfig::new("authors", false));
    let tabs_field = make_tabs_field(
        "layout",
        vec![
            FieldTab {
                label: "Content".to_string(),
                description: None,
                fields: vec![make_field("title", FieldType::Text)],
            },
            FieldTab {
                label: "Meta".to_string(),
                description: None,
                fields: vec![author_field],
            },
        ],
    );
    let posts_def = make_collection_def("posts", vec![tabs_field]);

    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(make_authors_def());

    let mut doc = Document::new("p1".to_string());
    doc.fields.insert("title".to_string(), json!("Hello"));
    doc.fields.insert("author".to_string(), json!("a1"));

    let mut visited = HashSet::new();
    populate_relationships_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut doc,
        &mut visited,
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

    let author = doc.fields.get("author").expect("author should exist");
    assert!(
        author.is_object(),
        "relationship inside tabs should be populated, got {:?}",
        author
    );
    assert_eq!(author.get("name").and_then(|v| v.as_str()), Some("Alice"));
}

#[test]
fn populate_relationship_inside_group() {
    let conn = setup_populate_db();

    let mut author_field = make_field("og_author", FieldType::Relationship);
    author_field.relationship = Some(RelationshipConfig::new("authors", false));
    let seo_field = make_group_field(
        "seo",
        vec![make_field("og_title", FieldType::Text), author_field],
    );
    let posts_def = make_collection_def(
        "posts",
        vec![make_field("title", FieldType::Text), seo_field],
    );

    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(make_authors_def());

    let mut doc = Document::new("p1".to_string());
    doc.fields.insert("title".to_string(), json!("Hello"));
    doc.fields.insert(
        "seo".to_string(),
        json!({"og_title": "Hello SEO", "og_author": "a1"}),
    );

    let mut visited = HashSet::new();
    populate_relationships_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut doc,
        &mut visited,
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

    let seo = doc.fields.get("seo").expect("seo should exist");
    let seo_obj = seo.as_object().expect("seo should be object");
    let og_author = seo_obj.get("og_author").expect("og_author should exist");
    assert!(
        og_author.is_object(),
        "relationship inside group should be populated, got {:?}",
        og_author
    );
    assert_eq!(
        og_author.get("name").and_then(|v| v.as_str()),
        Some("Alice")
    );
}

#[test]
fn populate_relationship_inside_array() {
    let conn = setup_populate_db();

    let mut ref_field = make_field("related", FieldType::Relationship);
    ref_field.relationship = Some(RelationshipConfig::new("authors", false));
    let items_field = FieldDefinition::builder("items", FieldType::Array)
        .fields(vec![make_field("label", FieldType::Text), ref_field])
        .build();
    let posts_def = make_collection_def(
        "posts",
        vec![make_field("title", FieldType::Text), items_field],
    );

    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(make_authors_def());

    let mut doc = Document::new("p1".to_string());
    doc.fields.insert("title".to_string(), json!("Hello"));
    doc.fields.insert(
        "items".to_string(),
        json!([
            {"id": "row1", "label": "First", "related": "a1"},
        ]),
    );

    let mut visited = HashSet::new();
    populate_relationships_cached(
        &PopulateContext {
            conn: &conn,
            registry: &registry,
            collection_slug: "posts",
            def: &posts_def,
        },
        &mut doc,
        &mut visited,
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

    let items = doc.fields.get("items").expect("items should exist");
    let arr = items.as_array().expect("items should be array");
    assert_eq!(arr.len(), 1);
    let row = arr[0].as_object().expect("row should be object");
    let related = row.get("related").expect("related should exist");
    assert!(
        related.is_object(),
        "relationship inside array should be populated, got {:?}",
        related
    );
    assert_eq!(related.get("name").and_then(|v| v.as_str()), Some("Alice"));
}
