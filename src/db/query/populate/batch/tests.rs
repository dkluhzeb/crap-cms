use std::collections::HashSet;

use anyhow::Result as AnyResult;
use rusqlite::Connection;
use serde_json::{Value, json};

use crate::core::cache::NoneCache;
use crate::core::{
    CollectionDefinition, Document, FieldType, HookRef, Registry, RelationshipConfig,
    VersionsConfig,
};
use crate::db::AccessResult;
use crate::db::query::populate::{
    JoinAccessCheck, PopulateContext, PopulateOpts, populate_relationships_batch,
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
            fields: &posts_def.fields,
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
            fields: &posts_def.fields,
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
            fields: &authors_def.fields,
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
        fields: &posts_def.fields,
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
                fields: &posts_def.fields,
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

// ── Shape parity with the single-document path ────────────────────────────

/// `posts` (`author`/`editor` → `users`, `related` → `posts`) holding `p1`
/// and `p2`, each authored and edited by `u1` and related to the other.
fn parity_fixture() -> (Connection, Registry, CollectionDefinition) {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE users (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);
         CREATE TABLE posts (
             id TEXT PRIMARY KEY, title TEXT, author TEXT, editor TEXT, related TEXT,
             created_at TEXT, updated_at TEXT
         );
         INSERT INTO users VALUES ('u1', 'Ada', '2024-01-01', '2024-01-01');
         INSERT INTO posts VALUES ('p1', 'One', 'u1', 'u1', 'p2', '2024-01-01', '2024-01-01');
         INSERT INTO posts VALUES ('p2', 'Two', 'u1', 'u1', 'p1', '2024-01-01', '2024-01-01');",
    )
    .unwrap();

    let rel = |name: &str, target: &str| {
        let mut field = make_field(name, FieldType::Relationship);
        field.relationship = Some(RelationshipConfig::new(target, false));
        field
    };

    let posts_def = make_collection_def(
        "posts",
        vec![
            make_field("title", FieldType::Text),
            rel("author", "users"),
            rel("editor", "users"),
            rel("related", "posts"),
        ],
    );

    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(make_collection_def(
        "users",
        vec![make_field("name", FieldType::Text)],
    ));

    (conn, registry, posts_def)
}

fn parity_seed(id: &str, related: &str) -> Document {
    let mut doc = Document::new(id.to_string());
    doc.fields.insert("title".to_string(), json!(id));
    doc.fields.insert("author".to_string(), json!("u1"));
    doc.fields.insert("editor".to_string(), json!("u1"));
    doc.fields.insert("related".to_string(), json!(related));
    doc
}

/// Regression: the single-document path's cycle guard was tree-global, so a
/// document populated in one branch stayed a bare id in a sibling branch
/// (`editor` after `author`), while the list path expanded it — `find` and
/// `find_by_id` returned different shapes for the same document. Both now
/// guard the ancestor path only: a document is expanded wherever it appears,
/// except as a reference back to one of its own ancestors.
#[test]
fn batch_and_single_paths_produce_the_same_shape_at_depth_two() {
    let (conn, registry, posts_def) = parity_fixture();
    let ctx = PopulateContext {
        conn: &conn,
        registry: &registry,
        collection_slug: "posts",
        fields: &posts_def.fields,
    };
    let opts = PopulateOpts {
        depth: 2,
        select: None,
        locale_ctx: None,
        published_only: false,
        join_access: None,
        user: None,
    };

    let mut batch = vec![parity_seed("p1", "p2"), parity_seed("p2", "p1")];
    populate_relationships_batch_cached(&ctx, &mut batch, &opts, &NoneCache).unwrap();

    for (i, (id, related)) in [("p1", "p2"), ("p2", "p1")].into_iter().enumerate() {
        let mut single = parity_seed(id, related);
        populate_relationships_cached(&ctx, &mut single, &mut HashSet::new(), &opts, &NoneCache)
            .unwrap();

        assert_eq!(
            single.fields, batch[i].fields,
            "{id}: list and by-id shapes differ"
        );
    }

    let p1 = &batch[0].fields;
    assert_eq!(p1["author"]["name"], json!("Ada"));
    assert_eq!(
        p1["editor"]["name"],
        json!("Ada"),
        "a sibling branch expands the same target"
    );
    assert_eq!(
        p1["related"]["id"],
        json!("p2"),
        "another document of the batch is not an ancestor"
    );
    assert_eq!(
        p1["related"]["related"],
        json!("p1"),
        "the way back to an ancestor stays an id"
    );
    assert_eq!(p1["related"]["author"]["name"], json!("Ada"));
}

// ── Draft reads show the target's pending draft ───────────────────────────

/// `read` allowed; `draft` (`draft_fn`) allowed only when `.0`.
struct DraftGate(bool);

impl JoinAccessCheck for DraftGate {
    fn check(
        &self,
        access: Option<&HookRef>,
        _: Option<&Document>,
        _: &str,
    ) -> AnyResult<AccessResult> {
        let is_draft = access.map(HookRef::reference) == Some("draft_fn");

        Ok(if is_draft && !self.0 {
            AccessResult::Denied
        } else {
            AccessResult::Allowed
        })
    }
}

/// A published `authors/a1` ("Published") with a pending draft edit ("Draft
/// edit"), referenced by `posts/p1`.
fn pending_draft_fixture() -> (Connection, Registry, CollectionDefinition) {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(&versions_table_sql("authors")).unwrap();
    conn.execute_batch(
        "CREATE TABLE authors (
             id TEXT PRIMARY KEY, name TEXT,
             _status TEXT NOT NULL DEFAULT 'published', created_at TEXT, updated_at TEXT
         );
         CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, author TEXT, created_at TEXT, updated_at TEXT);
         INSERT INTO authors VALUES ('a1', 'Published', 'published', '2024-01-01', '2024-01-01');
         INSERT INTO _versions_authors VALUES
             ('v1', 'a1', 1, 'published', 0, '{\"name\":\"Published\"}', '2024-01-01'),
             ('v2', 'a1', 2, 'draft', 1, '{\"name\":\"Draft edit\",\"_status\":\"draft\"}', '2024-01-02');
         INSERT INTO posts VALUES ('p1', 'Hello', 'a1', '2024-01-01', '2024-01-01');",
    )
    .unwrap();

    let mut authors_def = make_authors_def();
    authors_def.versions = Some(VersionsConfig::new(true, 0));
    authors_def.access.draft = Some(HookRef::new("draft_fn"));

    let posts_def = make_posts_def();
    let mut registry = Registry::new();
    registry.register_collection(posts_def.clone());
    registry.register_collection(authors_def);

    (conn, registry, posts_def)
}

/// The populated author as a read with `published_only` and `draft_allowed`
/// embeds it — through the list path, checked equal to the by-id path.
fn embedded_author(published_only: bool, draft_allowed: bool) -> Value {
    let (conn, registry, posts_def) = pending_draft_fixture();
    let gate = DraftGate(draft_allowed);
    let ctx = PopulateContext {
        conn: &conn,
        registry: &registry,
        collection_slug: "posts",
        fields: &posts_def.fields,
    };
    let opts = PopulateOpts {
        depth: 1,
        select: None,
        locale_ctx: None,
        published_only,
        join_access: Some(&gate),
        user: None,
    };
    let seed = || {
        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("author".to_string(), json!("a1"));
        doc
    };

    let mut batch = vec![seed()];
    populate_relationships_batch_cached(&ctx, &mut batch, &opts, &NoneCache).unwrap();

    let mut single = seed();
    populate_relationships_cached(&ctx, &mut single, &mut HashSet::new(), &opts, &NoneCache)
        .unwrap();

    assert_eq!(batch[0].fields, single.fields, "list and by-id agree");

    batch[0].fields["author"].clone()
}

/// A draft read embeds a target's pending draft where the reader may read
/// the target's drafts — as a draft read of the target by id shows it — and
/// its published row otherwise. The embedded `_status` stays the document's.
#[test]
fn draft_read_embeds_the_targets_pending_draft() {
    let draft = embedded_author(false, true);
    assert_eq!(draft["name"], json!("Draft edit"));
    assert_eq!(draft["_status"], json!("published"));

    assert_eq!(
        embedded_author(false, false)["name"],
        json!("Published"),
        "without the target's draft access, the published row"
    );
    assert_eq!(
        embedded_author(true, true)["name"],
        json!("Published"),
        "a published-only read never shows a draft"
    );
}
