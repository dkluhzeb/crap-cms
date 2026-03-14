//! Polymorphic batch population helpers.

use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use crate::{
    core::{Document, upload},
    db::query::{
        populate::{
            PopulateContext, PopulateCtx, PopulateOpts, document_to_json, parse_poly_ref,
            populate_relationships_batch_cached,
        },
        read::find_by_ids,
    },
};

/// Batch fetch and distribute for polymorphic has-many fields.
pub(super) fn batch_poly_has_many(
    ctx: &PopulateCtx<'_>,
    docs: &mut [Document],
    field_name: &str,
    visited: &HashSet<(String, String)>,
) -> Result<()> {
    // Collect all unique poly refs across all docs
    let mut ids_by_collection: HashMap<String, Vec<String>> = HashMap::new();
    for doc in docs.iter() {
        if let Some(Value::Array(arr)) = doc.fields.get(field_name) {
            for v in arr {
                if let Some(s) = v.as_str()
                    && let Some((col, id)) = parse_poly_ref(s)
                    && !visited.contains(&(col.clone(), id.clone()))
                {
                    ids_by_collection.entry(col).or_default().push(id);
                }
            }
        }
    }

    // Deduplicate IDs per collection
    for ids in ids_by_collection.values_mut() {
        ids.sort();
        ids.dedup();
    }

    // Batch fetch per collection
    let fetched_map = batch_fetch_with_cache(ctx, &ids_by_collection)?;

    // Distribute results back to each document
    for doc in docs.iter_mut() {
        let items: Vec<String> = match doc.fields.get(field_name) {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => continue,
        };
        let mut populated = Vec::new();
        for item in &items {
            if let Some((col, id)) = parse_poly_ref(item) {
                if let Some(col_map) = fetched_map.get(&col) {
                    if let Some(cached_doc) = col_map.get(&id) {
                        populated.push(document_to_json(cached_doc, &col));
                    } else {
                        populated.push(Value::String(item.clone()));
                    }
                } else {
                    populated.push(Value::String(item.clone()));
                }
            } else {
                populated.push(Value::String(item.clone()));
            }
        }
        doc.fields
            .insert(field_name.to_string(), Value::Array(populated));
    }
    Ok(())
}

/// Batch fetch and distribute for polymorphic has-one fields.
pub(super) fn batch_poly_has_one(
    ctx: &PopulateCtx<'_>,
    docs: &mut [Document],
    field_name: &str,
    visited: &HashSet<(String, String)>,
) -> Result<()> {
    let mut ids_by_collection: HashMap<String, Vec<String>> = HashMap::new();
    for doc in docs.iter() {
        if let Some(Value::String(s)) = doc.fields.get(field_name)
            && !s.is_empty()
            && let Some((col, id)) = parse_poly_ref(s)
            && !visited.contains(&(col.clone(), id.clone()))
        {
            ids_by_collection.entry(col).or_default().push(id);
        }
    }

    for ids in ids_by_collection.values_mut() {
        ids.sort();
        ids.dedup();
    }

    let fetched_map = batch_fetch_with_cache(ctx, &ids_by_collection)?;

    for doc in docs.iter_mut() {
        let raw = match doc.fields.get(field_name) {
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => continue,
        };

        if let Some((col, id)) = parse_poly_ref(&raw)
            && let Some(col_map) = fetched_map.get(&col)
            && let Some(cached_doc) = col_map.get(&id)
        {
            doc.fields
                .insert(field_name.to_string(), document_to_json(cached_doc, &col));
        }
    }
    Ok(())
}

/// Shared helper: fetch documents from multiple collections with cache support.
/// Used by polymorphic batch population.
pub(super) fn batch_fetch_with_cache(
    ctx: &PopulateCtx<'_>,
    ids_by_collection: &HashMap<String, Vec<String>>,
) -> Result<HashMap<String, HashMap<String, Document>>> {
    let mut fetched_map: HashMap<String, HashMap<String, Document>> = HashMap::new();
    for (col, col_ids) in ids_by_collection {
        if let Some(item_def) = ctx.registry.get_collection(col) {
            let item_def = item_def.clone();
            let mut doc_map: HashMap<String, Document> = HashMap::new();
            let mut uncached_ids: Vec<String> = Vec::new();
            for id in col_ids {
                let key = (col.clone(), id.clone());

                if let Some(cached) = ctx.cache.get(&key) {
                    doc_map.insert(id.clone(), cached.value().clone());
                } else {
                    uncached_ids.push(id.clone());
                }
            }

            if !uncached_ids.is_empty() {
                let mut fetched =
                    find_by_ids(ctx.conn, col, &item_def, &uncached_ids, ctx.locale_ctx)?;
                for d in &mut fetched {
                    if let Some(ref uc) = item_def.upload
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
                            collection_slug: col,
                            def: &item_def,
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
                    ctx.cache.insert((col.clone(), d.id.to_string()), d.clone());
                    doc_map.insert(d.id.to_string(), d);
                }
            }
            fetched_map.insert(col.clone(), doc_map);
        }
    }
    Ok(fetched_map)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::core::field::*;
    use crate::core::{Document, Registry};
    use crate::db::query::{
        PopulateCache, PopulateContext, PopulateOpts, join,
        populate::{populate_relationships_batch_cached, test_helpers::*},
    };

    // ── Polymorphic has-one (batch) ────────────────────────────────────────────

    #[test]
    fn batch_polymorphic_has_one() {
        let conn = setup_polymorphic_populate_db();
        let entries_def = make_entries_def_poly_has_one();
        let articles_def =
            make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
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
                def: &entries_def,
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
        let articles_def =
            make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
        let pages_def = make_collection_def("pages", vec![make_field("title", FieldType::Text)]);
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());
        registry.register_collection(articles_def);
        registry.register_collection(pages_def);

        let mut doc = Document::new("e1".to_string());
        doc.fields.insert("title".to_string(), json!("Entry"));
        join::hydrate_document(&conn, "entries", &entries_def.fields, &mut doc, None, None)
            .unwrap();

        let mut docs = vec![doc];
        populate_relationships_batch_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "entries",
                def: &entries_def,
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
        let articles_def =
            make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
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
                def: &entries_def,
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
        let articles_def =
            make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
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
                def: &entries_def,
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
    fn batch_polymorphic_has_many_doc_not_in_col_map_keeps_string() {
        let conn = setup_polymorphic_populate_db();
        let entries_def = make_entries_def_poly_has_many();
        // Register "articles" in registry so fetched_map gets an entry, but fetch
        // "nonexistent" id which won't be in the returned results
        let articles_def =
            make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());
        registry.register_collection(articles_def);

        let mut doc = Document::new("e1".to_string());
        // "articles" is a known collection, but "nope" doesn't exist in DB
        doc.fields
            .insert("refs".to_string(), json!(["articles/nope"]));

        let mut docs = vec![doc];
        populate_relationships_batch_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "entries",
                def: &entries_def,
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

        let refs = docs[0].fields.get("refs").expect("refs should exist");
        let arr = refs.as_array().expect("refs should be array");
        assert_eq!(arr.len(), 1);
        // articles is in fetched_map but "nope" was not found → stays as string
        assert_eq!(
            arr[0].as_str(),
            Some("articles/nope"),
            "missing doc in batch poly has-many should remain as string"
        );
    }

    // ── Polymorphic has-one: unknown collection in distribution ───────────────

    #[test]
    fn batch_polymorphic_has_one_unknown_col_in_distribution_keeps_string() {
        let conn = setup_polymorphic_populate_db();
        let entries_def = make_entries_def_poly_has_one();
        // Don't register "videos" in registry
        let articles_def =
            make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
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
                def: &entries_def,
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
        let articles_def =
            make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());
        registry.register_collection(articles_def);

        let mut doc = Document::new("e1".to_string());
        doc.fields
            .insert("related".to_string(), json!("articles/a1"));

        let cache = PopulateCache::new();
        let mut docs = vec![doc];

        // Pre-populate cache so the doc is returned from cache in distribution
        let mut cached_article = Document::new("a1".to_string());
        cached_article
            .fields
            .insert("title".to_string(), json!("CachedFromBatchCache"));
        cache.insert(("articles".to_string(), "a1".to_string()), cached_article);

        populate_relationships_batch_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "entries",
                def: &entries_def,
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

        let related = docs[0].fields.get("related").expect("related should exist");
        assert!(related.is_object(), "should be populated");
        assert_eq!(
            related.get("title").and_then(|v| v.as_str()),
            Some("CachedFromBatchCache"),
            "should use cached document in batch poly has-one"
        );
    }

    // ── Polymorphic has-one: cache hit in batch ───────────────────────────────

    #[test]
    fn batch_polymorphic_has_one_cache_hit() {
        let conn = setup_polymorphic_populate_db();
        let entries_def = make_entries_def_poly_has_one();
        let articles_def =
            make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());
        registry.register_collection(articles_def);

        // Pre-populate cache so find_by_ids is skipped for this id
        let cache = PopulateCache::new();
        let mut cached_article = Document::new("a1".to_string());
        cached_article
            .fields
            .insert("title".to_string(), json!("CachedTitle"));
        cache.insert(("articles".to_string(), "a1".to_string()), cached_article);

        let mut doc = Document::new("e1".to_string());
        doc.fields
            .insert("related".to_string(), json!("articles/a1"));
        let mut docs = vec![doc];

        populate_relationships_batch_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "entries",
                def: &entries_def,
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

        let related = docs[0].fields.get("related").expect("related should exist");
        assert!(related.is_object());
        assert_eq!(
            related.get("title").and_then(|v| v.as_str()),
            Some("CachedTitle"),
            "batch poly has-one should use cache"
        );
    }
}
