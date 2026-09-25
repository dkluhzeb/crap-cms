//! Batch population of join fields.

use std::collections::HashSet;

use anyhow::Result as AnyResult;
use serde_json::json;

use crate::core::{
    CollectionDefinition, Document, FieldDefinition, FieldType, HookRef, JoinConfig, Registry,
    RelationshipConfig, cache::NoneCache,
};
use crate::db::query::populate::test_helpers::{
    make_authors_def_with_join, make_posts_def_for_join, setup_join_db,
};
use crate::db::query::populate::{
    JoinAccessCheck, PopulateContext, PopulateOpts, batch::populate_relationships_batch_cached,
    populate_relationships_cached,
};
use crate::db::query::test_helpers::CountingConn;
use crate::db::{AccessResult, DbConnection as _, InMemoryConn};

/// Regression for the join-field N+1: batch populate across N parent docs
/// must produce correct per-parent buckets. Before this change, the code
/// issued one `find()` per parent; correctness was preserved but query
/// count scaled with N. After the fix: one `IN (…)` query per field,
/// results bucketed by the `on_field` value.
#[test]
fn batch_join_field_buckets_per_parent() {
    let conn = setup_join_db();
    let authors_def = make_authors_def_with_join();
    let posts_def = make_posts_def_for_join();

    let mut registry = Registry::new();
    registry.register_collection(authors_def.clone());
    registry.register_collection(posts_def);

    // Two authors as parents: a1 (has posts p1, p2), a2 (has post p3).
    let mut docs = vec![
        {
            let mut d = Document::new("a1".to_string());
            d.fields.insert("name".to_string(), json!("Alice"));
            d
        },
        {
            let mut d = Document::new("a2".to_string());
            d.fields.insert("name".to_string(), json!("Bob"));
            d
        },
    ];

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

    // a1 sees exactly its two posts.
    let a1_posts = docs[0]
        .fields
        .get("posts")
        .and_then(|v| v.as_array())
        .expect("a1 should get a posts array");
    assert_eq!(a1_posts.len(), 2, "a1 has 2 posts");
    let a1_titles: Vec<&str> = a1_posts
        .iter()
        .filter_map(|v| v.get("title").and_then(|t| t.as_str()))
        .collect();
    assert!(a1_titles.contains(&"First Post"));
    assert!(a1_titles.contains(&"Second Post"));

    // a2 sees only its own post — no leakage from a1.
    let a2_posts = docs[1]
        .fields
        .get("posts")
        .and_then(|v| v.as_array())
        .expect("a2 should get a posts array");
    assert_eq!(a2_posts.len(), 1, "a2 has 1 post");
    assert_eq!(
        a2_posts[0].get("title").and_then(|t| t.as_str()),
        Some("Other Post")
    );
}

/// Batch path: an author with no matching posts must get an empty array,
/// not a missing field. Before the batch rewrite this worked by accident
/// because each parent ran its own query; after the rewrite the bucket
/// lookup must still emit `[]` for no-match cases.
#[test]
fn batch_join_field_empty_bucket_for_parent_with_no_matches() {
    let conn = setup_join_db();
    let authors_def = make_authors_def_with_join();
    let posts_def = make_posts_def_for_join();

    let mut registry = Registry::new();
    registry.register_collection(authors_def.clone());
    registry.register_collection(posts_def);

    // a99 is a parent with no matching posts — the setup_join_db fixture
    // has no posts with author='a99'.
    let mut docs = vec![{
        let mut d = Document::new("a99".to_string());
        d.fields.insert("name".to_string(), json!("Nobody"));
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
        .and_then(|v| v.as_array())
        .expect("posts must still be present as an empty array");
    assert!(
        posts.is_empty(),
        "no-match parent must render as empty array, not missing"
    );
}

/// Regression (B2): join-children preparation is batched — ONE hydrate
/// query per join-shaped child field and ONE child-populate pass for the
/// whole batch, regardless of how many parents or children matched.
/// Before, `prepare_join_children` hydrated and populated each child in
/// a loop (one array query PER CHILD here).
#[test]
#[allow(clippy::too_many_lines)]
fn batch_join_children_hydrate_query_count_is_constant() {
    let conn = InMemoryConn::open();
    conn.execute_batch(
        "CREATE TABLE authors (
             id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT
         );
         CREATE TABLE posts (
             id TEXT PRIMARY KEY, title TEXT, author TEXT,
             created_at TEXT, updated_at TEXT
         );
         CREATE TABLE posts_sections (
             id TEXT PRIMARY KEY, parent_id TEXT, _order INTEGER, label TEXT
         );
         INSERT INTO posts VALUES
             ('p1', 'One', 'a1', '2024-01-01', '2024-01-01'),
             ('p2', 'Two', 'a1', '2024-01-01', '2024-01-01'),
             ('p3', 'Three', 'a2', '2024-01-01', '2024-01-01');
         INSERT INTO posts_sections VALUES
             ('s1', 'p1', 0, 'Intro'), ('s2', 'p2', 0, 'Body'), ('s3', 'p3', 0, 'End');",
    )
    .unwrap();

    let mut join_field = FieldDefinition::builder("posts", FieldType::Join).build();
    join_field.join = Some(JoinConfig::new("posts", "author"));
    let mut authors_def = CollectionDefinition::new("authors");
    authors_def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text).build(),
        join_field,
    ];

    let mut posts_def = CollectionDefinition::new("posts");
    posts_def.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        FieldDefinition::builder("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new("authors", false))
            .build(),
        FieldDefinition::builder("sections", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("label", FieldType::Text).build(),
            ])
            .build(),
    ];

    let mut registry = Registry::new();
    registry.register_collection(authors_def.clone());
    registry.register_collection(posts_def);

    let run = |parents: &[&str]| -> (usize, Vec<Document>) {
        let counting = CountingConn::new(&conn);
        let mut docs: Vec<Document> = parents
            .iter()
            .map(|id| {
                let mut d = Document::new((*id).to_string());
                d.fields.insert("name".to_string(), json!("x"));
                d
            })
            .collect();

        populate_relationships_batch_cached(
            &PopulateContext {
                conn: &counting,
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
        (counting.reads(), docs)
    };

    let (reads_one, _) = run(&["a1"]);
    let (reads_two, docs) = run(&["a1", "a2"]);

    // Correctness: children carry their hydrated array rows, per parent
    // (order-independent — the default sort ties on equal timestamps).
    let a1_posts = docs[0]
        .fields
        .get("posts")
        .and_then(|v| v.as_array())
        .unwrap();
    assert_eq!(a1_posts.len(), 2);
    let a1_labels: Vec<&str> = a1_posts
        .iter()
        .filter_map(|p| p["sections"][0]["label"].as_str())
        .collect();
    assert!(a1_labels.contains(&"Intro") && a1_labels.contains(&"Body"));
    let a2_posts = docs[1]
        .fields
        .get("posts")
        .and_then(|v| v.as_array())
        .unwrap();
    assert_eq!(a2_posts[0]["sections"][0]["label"], "End");

    assert_eq!(
        reads_one, reads_two,
        "join-children query count must not scale with parents/children"
    );
    assert_eq!(
        reads_two, 2,
        "one children find + one batched sections hydrate"
    );
}

/// SEC-G guardrail preserved across the batch: Denied target access must
/// leave every parent with an empty array, not an unfiltered fetch.
#[test]
fn batch_join_field_denies_for_all_parents_when_target_read_denied() {
    struct DenyAll;
    impl JoinAccessCheck for DenyAll {
        fn check(
            &self,
            _: Option<&HookRef>,
            _: Option<&Document>,
            _: &str,
        ) -> AnyResult<AccessResult> {
            Ok(AccessResult::Denied)
        }
    }

    let conn = setup_join_db();
    let authors_def = make_authors_def_with_join();
    let posts_def = make_posts_def_for_join();

    let mut registry = Registry::new();
    registry.register_collection(authors_def.clone());
    registry.register_collection(posts_def);

    let mut docs = vec![
        {
            let mut d = Document::new("a1".to_string());
            d.fields.insert("name".to_string(), json!("Alice"));
            d
        },
        {
            let mut d = Document::new("a2".to_string());
            d.fields.insert("name".to_string(), json!("Bob"));
            d
        },
    ];

    let deny = DenyAll;
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
            join_access: Some(&deny),
            user: None,
        },
        &NoneCache,
    )
    .unwrap();

    for (i, doc) in docs.iter().enumerate() {
        let arr = doc.fields.get("posts").and_then(|v| v.as_array()).unwrap();
        assert!(
            arr.is_empty(),
            "parent {i} must have empty posts under Denied"
        );
    }
}

/// `authors` whose `posts` join lists at most `limit` posts, with its posts.
fn limited_join_registry(limit: u32) -> (Registry, CollectionDefinition) {
    let mut authors_def = make_authors_def_with_join();
    if let Some(join) = authors_def.fields[1].join.as_mut() {
        join.limit = Some(limit);
    }

    let mut registry = Registry::new();
    registry.register_collection(authors_def.clone());
    registry.register_collection(make_posts_def_for_join());

    (registry, authors_def)
}

fn author(id: &str) -> Document {
    let mut doc = Document::new(id.to_string());
    doc.fields.insert("name".to_string(), json!(id));
    doc
}

fn join_opts() -> PopulateOpts<'static> {
    PopulateOpts {
        depth: 1,
        select: None,
        locale_ctx: None,
        published_only: false,
        join_access: None,
        user: None,
    }
}

fn joined_count(doc: &Document) -> usize {
    doc.fields
        .get("posts")
        .and_then(|v| v.as_array())
        .map_or(0, Vec::len)
}

/// Regression: a join listed every referencing document — unbounded, for
/// every parent of a list read and recursively at every depth. It lists at
/// most its `limit` per document, on the list path and the by-id path alike.
#[test]
fn join_lists_at_most_its_limit_per_document() {
    let conn = setup_join_db();
    let (registry, authors_def) = limited_join_registry(1);
    let ctx = PopulateContext {
        conn: &conn,
        registry: &registry,
        collection_slug: "authors",
        fields: &authors_def.fields,
    };

    // a1 has two posts, a2 one.
    let mut docs = vec![author("a1"), author("a2")];
    populate_relationships_batch_cached(&ctx, &mut docs, &join_opts(), &NoneCache).unwrap();

    assert_eq!(joined_count(&docs[0]), 1, "a1 is capped at the limit");
    assert_eq!(joined_count(&docs[1]), 1, "a2's own post is still listed");

    let mut single = author("a1");
    populate_relationships_cached(
        &ctx,
        &mut single,
        &mut HashSet::new(),
        &join_opts(),
        &NoneCache,
    )
    .unwrap();

    assert_eq!(joined_count(&single), 1);
}

/// Regression: a failed join lookup (a backend error) rendered as an empty
/// join — "no related documents" as data. It propagates, on both paths.
#[test]
fn join_lookup_errors_propagate() {
    let conn = InMemoryConn::open();
    conn.execute_batch(
        "CREATE TABLE authors (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);",
    )
    .unwrap();

    let (registry, authors_def) = limited_join_registry(5);
    let ctx = PopulateContext {
        conn: &conn,
        registry: &registry,
        collection_slug: "authors",
        fields: &authors_def.fields,
    };

    let mut docs = vec![author("a1")];
    assert!(
        populate_relationships_batch_cached(&ctx, &mut docs, &join_opts(), &NoneCache).is_err(),
        "the list path must not swallow the missing posts table"
    );

    let mut single = author("a1");
    assert!(
        populate_relationships_cached(
            &ctx,
            &mut single,
            &mut HashSet::new(),
            &join_opts(),
            &NoneCache
        )
        .is_err(),
        "the by-id path must not swallow it either"
    );
}

/// Regression: a join inside a row / collapsible / tabs wrapper was never
/// populated by a read (the key was absent) although the admin showed it.
#[test]
fn join_inside_a_layout_wrapper_is_populated() {
    let conn = setup_join_db();

    let join = FieldDefinition::builder("posts", FieldType::Join)
        .join(JoinConfig::new("posts", "author"))
        .build();
    let row = FieldDefinition::builder("layout", FieldType::Row)
        .fields(vec![join])
        .build();

    let mut authors_def = CollectionDefinition::new("authors");
    authors_def.fields = vec![
        FieldDefinition::builder("name", FieldType::Text).build(),
        row,
    ];

    let mut registry = Registry::new();
    registry.register_collection(authors_def.clone());
    registry.register_collection(make_posts_def_for_join());

    let ctx = PopulateContext {
        conn: &conn,
        registry: &registry,
        collection_slug: "authors",
        fields: &authors_def.fields,
    };

    let mut docs = vec![author("a1")];
    populate_relationships_batch_cached(&ctx, &mut docs, &join_opts(), &NoneCache).unwrap();
    assert_eq!(joined_count(&docs[0]), 2);

    let mut single = author("a1");
    populate_relationships_cached(
        &ctx,
        &mut single,
        &mut HashSet::new(),
        &join_opts(),
        &NoneCache,
    )
    .unwrap();
    assert_eq!(joined_count(&single), 2);
}
