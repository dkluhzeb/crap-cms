//! Non-polymorphic relationship population helpers.

use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use super::dispatch::{finalize_target, resolve_single_target};
use crate::core::{CollectionDefinition, Document};
use crate::db::query::populate::helpers::{
    cache_get_doc, cache_set_doc, fetch_targets, resolve_target_views,
};
use crate::db::query::populate::{
    PopulateCtx, document_to_json, locale_cache_key, populate_cache_key,
};

/// Populate a non-polymorphic has-many field.
pub(super) fn populate_nonpoly_has_many(
    ctx: &PopulateCtx<'_>,
    doc: &mut Document,
    field_name: &str,
    rel_collection: &str,
    rel_def: &CollectionDefinition,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let ids: Vec<String> = match doc.fields.get(field_name) {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
            .collect(),
        _ => return Ok(()),
    };

    // Resolve the target collection's view access (read + draft) once for the
    // whole field.
    let views = resolve_target_views(ctx, rel_collection, rel_def)?;
    let locale_key = locale_cache_key(ctx.locale_ctx);

    // Gather raw docs: serve cache hits, then one batched DB fetch for the rest.
    let mut raws: HashMap<String, Document> = HashMap::new();
    let mut to_fetch: Vec<String> = Vec::new();
    for id in &ids {
        if visited.contains(&(rel_collection.to_string(), id.clone())) {
            continue;
        }
        let key = populate_cache_key(rel_collection, id, locale_key.as_deref());
        if let Some(raw) = cache_get_doc(ctx.cache, &key)? {
            raws.insert(id.clone(), raw);
        } else {
            to_fetch.push(id.clone());
        }
    }
    if !to_fetch.is_empty() {
        for raw in fetch_targets(ctx, rel_collection, rel_def, &to_fetch)? {
            let key = populate_cache_key(rel_collection, raw.id.as_ref(), locale_key.as_deref());
            let _ = cache_set_doc(ctx.cache, &key, &raw);
            raws.insert(raw.id.to_string(), raw);
        }
    }

    let mut populated = Vec::new();
    for id in &ids {
        if visited.contains(&(rel_collection.to_string(), id.clone())) {
            populated.push(Value::String(id.clone()));
            continue;
        }

        // DB miss (truly missing): omit from the array.
        let Some(raw) = raws.remove(id) else {
            continue;
        };

        // Per-request draft + access filter, then populate. Hidden → omit.
        if let Some(target) = finalize_target(
            ctx,
            rel_collection,
            rel_def,
            raw,
            &views,
            ctx.effective_depth,
            visited,
        )? {
            populated.push(document_to_json(&target, rel_collection));
        }
    }

    doc.fields
        .insert(field_name.to_string(), Value::Array(populated));
    Ok(())
}

/// Populate a non-polymorphic has-one field.
pub(super) fn populate_nonpoly_has_one(
    ctx: &PopulateCtx<'_>,
    doc: &mut Document,
    field_name: &str,
    rel_collection: &str,
    rel_def: &CollectionDefinition,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let id = match doc.fields.get(field_name) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Ok(()),
    };

    if visited.contains(&(rel_collection.to_string(), id.clone())) {
        return Ok(());
    }

    let views = resolve_target_views(ctx, rel_collection, rel_def)?;

    match resolve_single_target(
        ctx,
        rel_collection,
        rel_def,
        &id,
        &views,
        ctx.effective_depth,
        visited,
    )? {
        Some(target) => {
            doc.fields.insert(
                field_name.to_string(),
                document_to_json(&target, rel_collection),
            );
        }
        None => {
            // Missing, or hidden by draft visibility / `read` access: null out.
            doc.fields.insert(field_name.to_string(), Value::Null);
        }
    }

    Ok(())
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::json;

    use super::super::populate_relationships_cached;
    use serde_json::Value;

    use crate::core::cache::{CacheBackend, MemoryCache, NoneCache};
    use crate::core::{Document, HookRef, Registry, field::*};
    use crate::db::query::populate::test_helpers::*;
    use crate::db::query::populate::{
        JoinAccessCheck, PopulateContext, PopulateOpts, populate_cache_key,
    };
    use crate::db::{AccessResult, Filter, FilterClause, FilterOp};
    use rusqlite::Connection;
    use std::collections::HashSet;

    #[test]
    fn populate_has_many_relationship() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE categories (
                id TEXT PRIMARY KEY,
                name TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE posts__tags (
                parent_id TEXT,
                related_id TEXT,
                position INTEGER
            );
            INSERT INTO categories (id, name, created_at, updated_at)
                VALUES ('c1', 'Tech', '2024-01-01', '2024-01-01');
            INSERT INTO categories (id, name, created_at, updated_at)
                VALUES ('c2', 'Science', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, created_at, updated_at)
                VALUES ('p1', 'Hello', '2024-01-01', '2024-01-01');
            INSERT INTO posts__tags (parent_id, related_id, position)
                VALUES ('p1', 'c1', 0);
            INSERT INTO posts__tags (parent_id, related_id, position)
                VALUES ('p1', 'c2', 1);",
        )
        .unwrap();

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

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), json!("Hello"));
        doc.fields.insert("tags".to_string(), json!(["c1", "c2"]));
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
                published_only: false,
                join_access: None,
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        let tags = doc.fields.get("tags").expect("tags field should exist");
        let arr = tags.as_array().expect("tags should be an array");
        assert_eq!(arr.len(), 2);
        assert!(
            arr[0].is_object(),
            "first tag should be populated as object"
        );
        assert_eq!(arr[0].get("name").and_then(|v| v.as_str()), Some("Tech"));
        assert!(
            arr[1].is_object(),
            "second tag should be populated as object"
        );
        assert_eq!(arr[1].get("name").and_then(|v| v.as_str()), Some("Science"));
    }

    #[test]
    fn populate_has_many_missing_related_is_dropped() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE categories (
                id TEXT PRIMARY KEY,
                name TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO categories (id, name, created_at, updated_at)
                VALUES ('c1', 'Tech', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, created_at, updated_at)
                VALUES ('p1', 'Hello', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

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

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), json!("Hello"));
        // One existing, two missing — the missing ones should be dropped from the array.
        doc.fields.insert(
            "tags".to_string(),
            json!(["nonexistent1", "c1", "nonexistent2"]),
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
                published_only: false,
                join_access: None,
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        let tags = doc.fields.get("tags").expect("tags should exist");
        let arr = tags.as_array().expect("tags should be an array");
        assert_eq!(arr.len(), 1, "missing targets should be dropped");
        assert!(arr[0].is_object(), "remaining tag should be populated");
        assert_eq!(arr[0].get("id").and_then(|v| v.as_str()), Some("c1"));
    }

    #[test]
    fn populate_has_many_soft_deleted_target_dropped() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE categories (
                id TEXT PRIMARY KEY,
                name TEXT,
                _deleted_at TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO categories (id, name, _deleted_at, created_at, updated_at)
                VALUES ('c1', 'Tech', NULL, '2024-01-01', '2024-01-01');
            INSERT INTO categories (id, name, _deleted_at, created_at, updated_at)
                VALUES ('c2', 'Science', '2024-02-01', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, created_at, updated_at)
                VALUES ('p1', 'Hello', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let mut cats_def =
            make_collection_def("categories", vec![make_field("name", FieldType::Text)]);
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

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), json!("Hello"));
        doc.fields.insert("tags".to_string(), json!(["c1", "c2"]));

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
                published_only: false,
                join_access: None,
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        let tags = doc.fields.get("tags").expect("tags should exist");
        let arr = tags.as_array().expect("tags should be an array");
        assert_eq!(arr.len(), 1, "soft-deleted target should be dropped");
        assert!(arr[0].is_object(), "remaining tag should be populated");
        assert_eq!(arr[0].get("name").and_then(|v| v.as_str()), Some("Tech"));
    }

    #[test]
    fn populate_has_one_missing_related_is_null() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE authors (
                id TEXT PRIMARY KEY,
                name TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                author TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO posts (id, title, author, created_at, updated_at)
                VALUES ('p1', 'Hello', 'missing', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("author".to_string(), json!("missing"));

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
                published_only: false,
                join_access: None,
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        assert_eq!(
            doc.fields.get("author"),
            Some(&serde_json::Value::Null),
            "missing has-one target should be null"
        );
    }

    /// Regression: a draft target must be hidden from population when the
    /// reader is not allowed to see drafts (`published_only`), and visible
    /// when drafts are allowed. Previously populate ignored `_status`
    /// entirely, leaking a never-published draft referenced by a published
    /// document to readers who could not see drafts directly.
    #[test]
    fn populate_has_one_draft_target_hidden_when_published_only() {
        use crate::core::VersionsConfig;

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE authors (
                id TEXT PRIMARY KEY,
                name TEXT,
                _status TEXT NOT NULL DEFAULT 'published',
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                author TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO authors (id, name, _status, created_at, updated_at)
                VALUES ('a1', 'Secret Draft', 'draft', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, author, created_at, updated_at)
                VALUES ('p1', 'Hello', 'a1', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let mut authors_def = make_authors_def();
        authors_def.versions = Some(VersionsConfig::new(true, 0)); // enables has_drafts()
        assert!(authors_def.has_drafts());

        let posts_def = make_posts_def();
        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(authors_def);

        let run = |published_only: bool| {
            let mut doc = Document::new("p1".to_string());
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
                    published_only,
                    join_access: None,
                    user: None,
                },
                &NoneCache,
            )
            .unwrap();
            doc.fields.get("author").cloned()
        };

        // Published-only read: the draft author is hidden (null).
        assert_eq!(
            run(true),
            Some(serde_json::Value::Null),
            "draft target must be hidden from a published-only read"
        );

        // Drafts allowed: the draft author is populated.
        let allowed = run(false).expect("author field present");
        assert!(
            allowed.is_object(),
            "draft target must be visible when drafts are allowed"
        );
        assert_eq!(
            allowed.get("name").and_then(|v| v.as_str()),
            Some("Secret Draft")
        );
    }

    /// Regression (P6.5): even when drafts are REQUESTED (`published_only =
    /// false`), a draft target is hidden unless the viewer holds the *target's*
    /// `draft` access. Draft visibility of a populated target is gated by the
    /// target collection's draft view, not merely the parent read's draft opt-in.
    #[test]
    fn populate_has_one_draft_target_gated_by_target_draft_access() {
        use crate::core::VersionsConfig;

        // read → Allowed; draft (`draft_fn`) → configurable.
        struct DraftGate(bool);
        impl JoinAccessCheck for DraftGate {
            fn check(
                &self,
                access: Option<&HookRef>,
                _: Option<&Document>,
                _: &str,
            ) -> anyhow::Result<AccessResult> {
                let is_draft = access.map(HookRef::reference) == Some("draft_fn");
                Ok(if is_draft && !self.0 {
                    AccessResult::Denied
                } else {
                    AccessResult::Allowed
                })
            }
        }

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE authors (
                id TEXT PRIMARY KEY, name TEXT,
                _status TEXT NOT NULL DEFAULT 'published', created_at TEXT, updated_at TEXT
            );
            CREATE TABLE posts (
                id TEXT PRIMARY KEY, title TEXT, author TEXT, created_at TEXT, updated_at TEXT
            );
            INSERT INTO authors (id, name, _status, created_at, updated_at)
                VALUES ('a1', 'Secret Draft', 'draft', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, author, created_at, updated_at)
                VALUES ('p1', 'Hello', 'a1', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let mut authors_def = make_authors_def();
        authors_def.versions = Some(VersionsConfig::new(true, 0));
        authors_def.access.read = Some(HookRef::new("read_fn"));
        authors_def.access.draft = Some(HookRef::new("draft_fn"));

        let posts_def = make_posts_def();
        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(authors_def);

        let run = |draft_allowed: bool| {
            let gate = DraftGate(draft_allowed);
            let mut doc = Document::new("p1".to_string());
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
                    published_only: false, // drafts requested
                    join_access: Some(&gate),
                    user: None,
                },
                &NoneCache,
            )
            .unwrap();
            doc.fields.get("author").cloned()
        };

        // Drafts requested but target draft access denied → still hidden.
        assert_eq!(
            run(false),
            Some(serde_json::Value::Null),
            "draft target must be hidden without the target's draft access"
        );

        // Target draft access granted → visible.
        assert!(
            run(true).is_some_and(|v| v.is_object()),
            "draft target visible once the target grants draft access"
        );
    }

    #[test]
    fn populate_has_one_soft_deleted_target_null() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE authors (
                id TEXT PRIMARY KEY,
                name TEXT,
                _deleted_at TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                author TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO authors (id, name, _deleted_at, created_at, updated_at)
                VALUES ('a1', 'Alice', '2024-02-01', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, author, created_at, updated_at)
                VALUES ('p1', 'Hello', 'a1', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let mut authors_def = make_authors_def();
        authors_def.soft_delete = true;

        let posts_def = make_posts_def();
        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(authors_def);

        let mut doc = Document::new("p1".to_string());
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
                published_only: false,
                join_access: None,
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        assert_eq!(
            doc.fields.get("author"),
            Some(&serde_json::Value::Null),
            "soft-deleted has-one target should be null"
        );
    }

    #[test]
    fn populate_has_many_visited_keeps_as_id() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE categories (
                id TEXT PRIMARY KEY,
                name TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO categories (id, name, created_at, updated_at)
                VALUES ('c1', 'Tech', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, created_at, updated_at)
                VALUES ('p1', 'Hello', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

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

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("tags".to_string(), json!(["c1"]));

        // Pre-mark c1 as visited — should keep it as ID string
        let mut visited = HashSet::new();
        visited.insert(("categories".to_string(), "c1".to_string()));

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
                published_only: false,
                join_access: None,
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        let tags = doc.fields.get("tags").expect("tags should exist");
        let arr = tags.as_array().expect("tags should be array");
        assert_eq!(arr.len(), 1);
        // Already visited — should remain as string ID
        assert_eq!(arr[0].as_str(), Some("c1"));
    }

    #[test]
    fn populate_has_one_visited_stops_population() {
        let conn = setup_populate_db();
        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("author".to_string(), json!("a1"));

        // Mark a1 as already visited
        let mut visited = HashSet::new();
        visited.insert(("authors".to_string(), "a1".to_string()));

        let cache = NoneCache;
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
                published_only: false,
                join_access: None,
                user: None,
            },
            &cache,
        )
        .unwrap();

        // author remains as ID because it's visited
        assert_eq!(
            doc.fields.get("author").and_then(|v| v.as_str()),
            Some("a1"),
            "visited has-one should not be populated"
        );
    }

    #[test]
    fn populate_has_one_cache_hit() {
        let conn = setup_populate_db();
        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        // Pre-populate cache with a different name to distinguish from DB
        let cache = MemoryCache::new(10_000);
        let mut cached_author = Document::new("a1".to_string());
        cached_author
            .fields
            .insert("name".to_string(), json!("CachedAlice"));
        cache
            .set(
                &populate_cache_key("authors", "a1", None),
                &serde_json::to_vec(&cached_author).unwrap(),
            )
            .unwrap();

        let mut doc = Document::new("p1".to_string());
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
                published_only: false,
                join_access: None,
                user: None,
            },
            &cache,
        )
        .unwrap();

        // Should use cache, not DB value
        let author = doc.fields.get("author").expect("author should exist");
        assert!(author.is_object(), "should be populated from cache");
        assert_eq!(
            author.get("name").and_then(|v| v.as_str()),
            Some("CachedAlice"),
            "should use cached document, not DB version"
        );
    }

    #[test]
    fn populate_has_many_cache_hit_in_reassembly() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE categories (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, created_at TEXT, updated_at TEXT);
             INSERT INTO categories VALUES ('c1', 'DBTech', '2024-01-01', '2024-01-01');
             INSERT INTO posts VALUES ('p1', 'Hello', '2024-01-01', '2024-01-01');"
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

        // Pre-populate cache with a different name to distinguish from DB
        let cache = MemoryCache::new(10_000);
        let mut cached_cat = Document::new("c1".to_string());
        cached_cat
            .fields
            .insert("name".to_string(), json!("CachedTech"));
        cache
            .set(
                &populate_cache_key("categories", "c1", None),
                &serde_json::to_vec(&cached_cat).unwrap(),
            )
            .unwrap();

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("tags".to_string(), json!(["c1"]));

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
                published_only: false,
                join_access: None,
                user: None,
            },
            &cache,
        )
        .unwrap();

        let tags = doc.fields.get("tags").expect("tags should exist");
        let arr = tags.as_array().expect("tags should be array");
        assert_eq!(arr.len(), 1);
        assert!(arr[0].is_object(), "should be populated from cache");
        assert_eq!(
            arr[0].get("name").and_then(|v| v.as_str()),
            Some("CachedTech"),
            "should use cached document, not DB version"
        );
    }

    // ── Relationship target read-access (Option 3 leak fix) ───────────────────

    struct DenyAll;
    impl JoinAccessCheck for DenyAll {
        fn check(
            &self,
            _: Option<&HookRef>,
            _: Option<&Document>,
            _: &str,
        ) -> anyhow::Result<AccessResult> {
            Ok(AccessResult::Denied)
        }
    }

    struct AllowAll;
    impl JoinAccessCheck for AllowAll {
        fn check(
            &self,
            _: Option<&HookRef>,
            _: Option<&Document>,
            _: &str,
        ) -> anyhow::Result<AccessResult> {
            Ok(AccessResult::Allowed)
        }
    }

    /// Constrain the target collection to rows whose `name` equals the value.
    struct ConstrainName(&'static str);
    impl JoinAccessCheck for ConstrainName {
        fn check(
            &self,
            _: Option<&HookRef>,
            _: Option<&Document>,
            _: &str,
        ) -> anyhow::Result<AccessResult> {
            Ok(AccessResult::Constrained(vec![FilterClause::Single(
                Filter {
                    field: "name".to_string(),
                    op: FilterOp::Equals(self.0.to_string()),
                },
            )]))
        }
    }

    /// Populate `p1`'s `author` has-one (→ `authors`, a1 = "Alice") under the
    /// given access check and cache, returning the parent doc.
    fn populate_author(join_access: &dyn JoinAccessCheck, cache: &dyn CacheBackend) -> Document {
        let conn = setup_populate_db();
        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        let mut doc = Document::new("p1".to_string());
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
                published_only: false,
                join_access: Some(join_access),
                user: None,
            },
            cache,
        )
        .unwrap();
        doc
    }

    /// A relationship target on a collection the reader cannot read must be
    /// nulled — not exfiltrated through population (the depth>0 leak this fix
    /// closes). Join fields already did this; relationship fields now match.
    #[test]
    fn relationship_has_one_denied_is_nulled() {
        let doc = populate_author(&DenyAll, &NoneCache);
        assert_eq!(
            doc.fields.get("author"),
            Some(&Value::Null),
            "denied relationship target must be null, not leaked"
        );
    }

    #[test]
    fn relationship_has_one_allowed_populates() {
        let doc = populate_author(&AllowAll, &NoneCache);
        assert!(
            doc.fields
                .get("author")
                .is_some_and(serde_json::Value::is_object),
            "allowed relationship target populates"
        );
    }

    /// THE cross-user isolation test: user A's allowed read caches the RAW
    /// author; user B, denied, reads the SAME cache and must NOT inherit A's
    /// view. Access is re-evaluated per request even on a cache hit — the whole
    /// reason the cache stores raw, user-independent content.
    #[test]
    fn cross_user_cache_isolation_no_leak() {
        let cache = MemoryCache::new(10_000);

        let doc_a = populate_author(&AllowAll, &cache);
        assert!(
            doc_a
                .fields
                .get("author")
                .is_some_and(serde_json::Value::is_object),
            "user A (allowed) sees the author"
        );

        let doc_b = populate_author(&DenyAll, &cache);
        assert_eq!(
            doc_b.fields.get("author"),
            Some(&Value::Null),
            "user B (denied) must not inherit user A's cached author view"
        );
    }

    /// A `Constrained` access decision is matched against the target's RAW
    /// fields (a1's `name` is "Alice").
    #[test]
    fn relationship_constrained_matches_raw_fields() {
        let visible = populate_author(&ConstrainName("Alice"), &NoneCache);
        assert!(
            visible
                .fields
                .get("author")
                .is_some_and(serde_json::Value::is_object),
            "constraint matching the raw author keeps it"
        );

        let hidden = populate_author(&ConstrainName("Bob"), &NoneCache);
        assert_eq!(
            hidden.fields.get("author"),
            Some(&Value::Null),
            "constraint not matching the raw author hides it"
        );
    }

    /// Depth-2 recursion forwards the access check: posts → author (allowed) →
    /// team (denied). The author populates, but its nested `team` is nulled —
    /// proving the recursion no longer passes `join_access: None`.
    #[test]
    fn nested_depth_two_target_denied() {
        struct DenyTeams;
        impl JoinAccessCheck for DenyTeams {
            fn check(
                &self,
                _: Option<&HookRef>,
                _: Option<&Document>,
                collection: &str,
            ) -> anyhow::Result<AccessResult> {
                if collection == "teams" {
                    Ok(AccessResult::Denied)
                } else {
                    Ok(AccessResult::Allowed)
                }
            }
        }

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE teams (id TEXT PRIMARY KEY, name TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE authors (id TEXT PRIMARY KEY, name TEXT, team TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, author TEXT, created_at TEXT, updated_at TEXT);
             INSERT INTO teams VALUES ('t1', 'Core', '2024-01-01', '2024-01-01');
             INSERT INTO authors VALUES ('a1', 'Alice', 't1', '2024-01-01', '2024-01-01');
             INSERT INTO posts VALUES ('p1', 'Hello', 'a1', '2024-01-01', '2024-01-01');",
        )
        .unwrap();

        let teams_def = make_collection_def("teams", vec![make_field("name", FieldType::Text)]);
        let mut team_field = make_field("team", FieldType::Relationship);
        team_field.relationship = Some(RelationshipConfig::new("teams", false));
        let authors_def = make_collection_def(
            "authors",
            vec![make_field("name", FieldType::Text), team_field],
        );
        let mut author_field = make_field("author", FieldType::Relationship);
        author_field.relationship = Some(RelationshipConfig::new("authors", false));
        let posts_def = make_collection_def(
            "posts",
            vec![make_field("title", FieldType::Text), author_field],
        );

        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(authors_def);
        registry.register_collection(teams_def);

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("author".to_string(), json!("a1"));

        let mut visited = HashSet::new();
        let deny_teams = DenyTeams;
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
                depth: 2,
                select: None,
                locale_ctx: None,
                published_only: false,
                join_access: Some(&deny_teams),
                user: None,
            },
            &NoneCache,
        )
        .unwrap();

        let author = doc.fields.get("author").expect("author present");
        assert!(
            author.is_object(),
            "depth-1 author is allowed and populates"
        );
        assert_eq!(
            author.get("team"),
            Some(&Value::Null),
            "depth-2 team is denied → null (recursion forwards the access check)"
        );
    }
}
