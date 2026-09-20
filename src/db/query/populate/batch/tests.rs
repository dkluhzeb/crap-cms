use std::collections::HashSet;

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::core::cache::NoneCache;
use crate::core::{CollectionDefinition, Document, FieldType, Registry, RelationshipConfig};
use crate::db::query::populate::{
    PopulateContext, PopulateOpts, populate_relationships_batch,
    populate_relationships_batch_cached, populate_relationships_cached, test_helpers::*,
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
            published_only: false,
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
            published_only: false,
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
            published_only: false,
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
            published_only: false,
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
            published_only: false,
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
            published_only: false,
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
            published_only: false,
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

// ── Cycle guard across the relationship recursion ─────────────────────────

/// Two collections pointing at each other: a post's best comment, and that
/// comment's post.
fn mutual_has_one_registry() -> (Registry, CollectionDefinition) {
    let mut best = make_field("best_comment", FieldType::Relationship);
    best.relationship = Some(RelationshipConfig::new("comments", false));
    let posts_def = make_collection_def("posts", vec![make_field("title", FieldType::Text), best]);

    let mut post_ref = make_field("post", FieldType::Relationship);
    post_ref.relationship = Some(RelationshipConfig::new("posts", false));
    let comments_def = make_collection_def(
        "comments",
        vec![make_field("body", FieldType::Text), post_ref],
    );

    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(comments_def);

    (registry, posts_def)
}

fn mutual_has_one_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, best_comment TEXT, created_at TEXT, updated_at TEXT);
         CREATE TABLE comments (id TEXT PRIMARY KEY, body TEXT, post TEXT, created_at TEXT, updated_at TEXT);
         INSERT INTO posts VALUES ('p1', 'Post', 'c1', '2024-01-01', '2024-01-01');
         INSERT INTO comments VALUES ('c1', 'Nice', 'p1', '2024-01-01', '2024-01-01');",
    )
    .unwrap();
    conn
}

/// Regression: the batch path's relationship recursion re-entered the PUBLIC
/// batch entry, which seeds a FRESH cycle guard — so a mutual reference
/// expanded again at every remaining depth level. The same `(collection, id)`
/// appeared twice on one path, and a list read disagreed with the
/// single-document read of the very same row.
#[test]
fn batch_mutual_has_one_stops_at_the_first_repeat() {
    let conn = mutual_has_one_db();
    let (registry, posts_def) = mutual_has_one_registry();

    let seed = || {
        let mut d = Document::new("p1".to_string());
        d.fields.insert("title".to_string(), json!("Post"));
        d.fields.insert("best_comment".to_string(), json!("c1"));
        d
    };
    let opts = || PopulateOpts {
        depth: 3,
        select: None,
        locale_ctx: None,
        published_only: false,
        join_access: None,
        user: None,
    };
    let ctx = PopulateContext {
        conn: &conn,
        registry: &registry,
        collection_slug: "posts",
        def: &posts_def,
    };

    let mut docs = vec![seed()];
    populate_relationships_batch_cached(&ctx, &mut docs, &opts(), &NoneCache).unwrap();

    let best = &docs[0].fields["best_comment"];
    assert_eq!(
        best.get("id").and_then(|v| v.as_str()),
        Some("c1"),
        "the comment is populated: {best}"
    );
    assert_eq!(
        best.get("post").and_then(|v| v.as_str()),
        Some("p1"),
        "the way back to a document already on the path stays a raw id: {best}"
    );

    // The single-document path (what FindByID runs) inherits its guard
    // correctly and is the reference answer for the same row.
    let mut single = seed();
    populate_relationships_cached(&ctx, &mut single, &mut HashSet::new(), &opts(), &NoneCache)
        .unwrap();

    assert_eq!(
        single.fields["best_comment"], docs[0].fields["best_comment"],
        "a list read must agree with the single-document read"
    );
}

/// Every `(collection, id)` appears at most once on any root-to-leaf path,
/// whatever the requested depth — the guard is inherited, not restarted.
#[test]
fn batch_cycle_guard_holds_at_every_depth() {
    let conn = mutual_has_one_db();
    let (registry, posts_def) = mutual_has_one_registry();

    for depth in 1..=5 {
        let mut docs = vec![{
            let mut d = Document::new("p1".to_string());
            d.fields.insert("title".to_string(), json!("Post"));
            d.fields.insert("best_comment".to_string(), json!("c1"));
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
                depth,
                select: None,
                locale_ctx: None,
                published_only: false,
                join_access: None,
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        // A second expansion of the post would nest another `best_comment`
        // object under the comment's `post`.
        let best = &docs[0].fields["best_comment"];
        assert!(
            best.get("post").is_none_or(Value::is_string),
            "depth {depth}: the ancestor was expanded a second time: {best}"
        );
    }
}
