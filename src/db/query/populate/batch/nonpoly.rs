//! Non-polymorphic batch population helpers.

use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use super::populate_relationships_batch_cached;
use crate::db::query::populate::{
    MAX_POPULATE_CACHE_SIZE, PopulateContext, PopulateCtx, PopulateOpts, document_to_json,
    locale_cache_key,
};
use crate::{
    core::{CollectionDefinition, Document, upload},
    db::query::read::find_by_ids,
};

/// Batch fetch and distribute for non-polymorphic has-many fields.
pub(super) fn batch_nonpoly_has_many(
    ctx: &PopulateCtx<'_>,
    docs: &mut [Document],
    field_name: &str,
    rel_collection: &str,
    rel_def: &CollectionDefinition,
    visited: &HashSet<(String, String)>,
) -> Result<()> {
    // Collect all unique IDs across all docs for this has-many field
    let mut all_ids: Vec<String> = Vec::new();
    for doc in docs.iter() {
        if let Some(Value::Array(arr)) = doc.fields.get(field_name) {
            for v in arr {
                if let Some(s) = v.as_str()
                    && !visited.contains(&(rel_collection.to_string(), s.to_string()))
                {
                    all_ids.push(s.to_string());
                }
            }
        }
    }
    all_ids.sort();
    all_ids.dedup();

    let doc_map = batch_fetch_single_collection(ctx, rel_collection, rel_def, &all_ids)?;

    // Distribute back to each document preserving order
    for doc in docs.iter_mut() {
        let ids: Vec<String> = match doc.fields.get(field_name) {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => continue,
        };
        let mut populated = Vec::new();
        for id in &ids {
            if let Some(cached_doc) = doc_map.get(id) {
                populated.push(document_to_json(cached_doc, rel_collection));
            } else {
                populated.push(Value::String(id.clone()));
            }
        }
        doc.fields
            .insert(field_name.to_string(), Value::Array(populated));
    }
    Ok(())
}

/// Batch fetch and distribute for non-polymorphic has-one fields.
pub(super) fn batch_nonpoly_has_one(
    ctx: &PopulateCtx<'_>,
    docs: &mut [Document],
    field_name: &str,
    rel_collection: &str,
    rel_def: &CollectionDefinition,
    visited: &HashSet<(String, String)>,
) -> Result<()> {
    let mut all_ids: Vec<String> = Vec::new();
    for doc in docs.iter() {
        if let Some(Value::String(s)) = doc.fields.get(field_name)
            && !s.is_empty()
            && !visited.contains(&(rel_collection.to_string(), s.clone()))
        {
            all_ids.push(s.clone());
        }
    }
    all_ids.sort();
    all_ids.dedup();

    let doc_map = batch_fetch_single_collection(ctx, rel_collection, rel_def, &all_ids)?;

    // Distribute back
    for doc in docs.iter_mut() {
        let id = match doc.fields.get(field_name) {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => continue,
        };

        if let Some(cached_doc) = doc_map.get(&id) {
            doc.fields.insert(
                field_name.to_string(),
                document_to_json(cached_doc, rel_collection),
            );
        }
    }
    Ok(())
}

/// Shared helper: fetch documents from a single collection with cache support.
/// Used by non-polymorphic batch population.
pub(super) fn batch_fetch_single_collection(
    ctx: &PopulateCtx<'_>,
    collection: &str,
    rel_def: &CollectionDefinition,
    all_ids: &[String],
) -> Result<HashMap<String, Document>> {
    let mut doc_map: HashMap<String, Document> = HashMap::new();
    let mut uncached_ids: Vec<String> = Vec::new();
    for id in all_ids {
        let key = (
            collection.to_string(),
            id.clone(),
            locale_cache_key(ctx.locale_ctx),
        );

        if let Some(cached) = ctx.cache.get(&key) {
            doc_map.insert(id.clone(), cached.value().clone());
        } else {
            uncached_ids.push(id.clone());
        }
    }

    if !uncached_ids.is_empty() {
        let mut fetched =
            find_by_ids(ctx.conn, collection, rel_def, &uncached_ids, ctx.locale_ctx)?;
        for d in &mut fetched {
            if let Some(ref uc) = rel_def.upload
                && uc.enabled
            {
                upload::assemble_sizes_object(d, uc);
            }
        }
        if ctx.effective_depth - 1 > 0 {
            populate_relationships_batch_cached(
                &PopulateContext {
                    conn: ctx.conn,
                    registry: ctx.registry,
                    collection_slug: collection,
                    def: rel_def,
                },
                &mut fetched,
                &PopulateOpts {
                    depth: ctx.effective_depth - 1,
                    select: None,
                    locale_ctx: ctx.locale_ctx,
                },
                ctx.cache,
            )?;
        }
        for d in fetched {
            if ctx.cache.len() < MAX_POPULATE_CACHE_SIZE {
                ctx.cache.insert(
                    (
                        collection.to_string(),
                        d.id.to_string(),
                        locale_cache_key(ctx.locale_ctx),
                    ),
                    d.clone(),
                );
            }
            doc_map.insert(d.id.to_string(), d);
        }
    }
    Ok(doc_map)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::super::test_helpers::*;
    use super::super::super::{PopulateCache, PopulateContext, PopulateOpts};
    use super::super::populate_relationships_batch_cached;
    use crate::core::field::*;
    use crate::core::{Document, Registry};
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
                def: &posts_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &PopulateCache::new(),
        )
        .unwrap();

        // All three should have populated authors
        for (i, doc) in docs.iter().enumerate() {
            let author = doc
                .fields
                .get("author")
                .unwrap_or_else(|| panic!("doc {} missing author", i));
            assert!(
                author.is_object(),
                "doc {} author should be object, got {:?}",
                i,
                author
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
                def: &posts_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &PopulateCache::new(),
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
        let cache = PopulateCache::new();
        let mut cached_author = Document::new("a1".to_string());
        cached_author
            .fields
            .insert("name".to_string(), json!("CachedBatchAuthor"));
        cache.insert(
            ("authors".to_string(), "a1".to_string(), None),
            cached_author,
        );

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
        let cache = PopulateCache::new();
        let mut cached_cat = Document::new("c1".to_string());
        cached_cat
            .fields
            .insert("name".to_string(), json!("CachedCategory"));
        cache.insert(
            ("categories".to_string(), "c1".to_string(), None),
            cached_cat,
        );

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
                def: &posts_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
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
                def: &posts_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &PopulateCache::new(),
        )
        .unwrap();

        // Field unchanged — unknown collection causes `continue`
        assert_eq!(
            docs[0].fields.get("author").and_then(|v| v.as_str()),
            Some("a1")
        );
    }

    #[test]
    fn batch_has_many_unknown_collection_skips() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (id TEXT PRIMARY KEY, title TEXT, created_at TEXT, updated_at TEXT);
             INSERT INTO posts VALUES ('p1', 'Post', '2024-01-01', '2024-01-01');"
        ).unwrap();

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
                def: &posts_def,
            },
            &mut docs,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &PopulateCache::new(),
        )
        .unwrap();

        // tags field unchanged
        let tags = docs[0].fields.get("tags").expect("tags should exist");
        assert!(tags.as_array().is_some());
    }
}
