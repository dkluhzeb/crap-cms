//! Non-polymorphic relationship population helpers.

use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use super::populate_relationships_cached;
use crate::db::query::populate::{
    PopulateContext, PopulateCtx, PopulateOpts, document_to_json, locale_cache_key,
    populate_cache_key,
};
use crate::{
    core::{CollectionDefinition, Document, upload},
    db::query::read::{find_by_id, find_by_ids},
};

use crate::db::query::populate::helpers::{
    CacheOrFetch, cache_get_doc, cache_or_fetch_doc, cache_set_doc,
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
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => return Ok(()),
    };

    let fetch_ids: Vec<String> = ids
        .iter()
        .filter(|id| !visited.contains(&(rel_collection.to_string(), id.to_string())))
        .cloned()
        .collect();

    let fetched = find_by_ids(
        ctx.conn,
        rel_collection,
        rel_def,
        &fetch_ids,
        ctx.locale_ctx,
    )?;
    let mut fetched_map: HashMap<String, Document> =
        fetched.into_iter().map(|d| (d.id.to_string(), d)).collect();

    let mut populated = Vec::new();
    for id in &ids {
        if visited.contains(&(rel_collection.to_string(), id.clone())) {
            populated.push(Value::String(id.clone()));
            continue;
        }

        let locale_key = locale_cache_key(ctx.locale_ctx);
        let key = populate_cache_key(rel_collection, id, locale_key.as_deref());

        if let Some(cached) = cache_get_doc(ctx.cache, &key)? {
            populated.push(document_to_json(&cached, rel_collection));
            continue;
        }

        // DB miss (soft-deleted or truly missing): omit from the array.
        let Some(mut related_doc) = fetched_map.remove(id) else {
            continue;
        };

        if let Some(ref uc) = rel_def.upload
            && uc.enabled
        {
            upload::assemble_sizes_object(&mut related_doc, uc);
        }
        populate_relationships_cached(
            &PopulateContext {
                conn: ctx.conn,
                registry: ctx.registry,
                collection_slug: rel_collection,
                def: rel_def,
            },
            &mut related_doc,
            visited,
            &PopulateOpts {
                depth: ctx.effective_depth - 1,
                select: None,
                locale_ctx: ctx.locale_ctx,
                join_access: None,
                user: None,
            },
            ctx.cache,
        )?;

        let _ = cache_set_doc(ctx.cache, &key, &related_doc);
        populated.push(document_to_json(&related_doc, rel_collection));
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

    let locale_key = locale_cache_key(ctx.locale_ctx);
    let key = populate_cache_key(rel_collection, &id, locale_key.as_deref());

    let mut related_doc = match cache_or_fetch_doc(ctx.cache, ctx.singleflight, &key, || {
        find_by_id(ctx.conn, rel_collection, rel_def, &id, ctx.locale_ctx)
            .ok()
            .flatten()
    }) {
        CacheOrFetch::Hit(cached) => {
            doc.fields.insert(
                field_name.to_string(),
                document_to_json(&cached, rel_collection),
            );
            return Ok(());
        }
        CacheOrFetch::Fresh(Some(d)) => d,
        CacheOrFetch::Fresh(None) => {
            // DB miss (soft-deleted or truly missing): set the field to null.
            doc.fields.insert(field_name.to_string(), Value::Null);
            return Ok(());
        }
    };

    if let Some(ref uc) = rel_def.upload
        && uc.enabled
    {
        upload::assemble_sizes_object(&mut related_doc, uc);
    }

    populate_relationships_cached(
        &PopulateContext {
            conn: ctx.conn,
            registry: ctx.registry,
            collection_slug: rel_collection,
            def: rel_def,
        },
        &mut related_doc,
        visited,
        &PopulateOpts {
            depth: ctx.effective_depth - 1,
            select: None,
            locale_ctx: ctx.locale_ctx,
            join_access: None,
            user: None,
        },
        ctx.cache,
    )?;

    // Overwrite with the populated version so future cache hits skip the
    // recursive population work.
    let _ = cache_set_doc(ctx.cache, &key, &related_doc);
    doc.fields.insert(
        field_name.to_string(),
        document_to_json(&related_doc, rel_collection),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::super::test_helpers::*;
    use super::super::super::{PopulateContext, PopulateOpts, populate_cache_key};
    use super::populate_relationships_cached;
    use crate::core::cache::{CacheBackend, MemoryCache, NoneCache};
    use crate::core::{Document, Registry, field::*};
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
}
