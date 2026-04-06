//! Polymorphic relationship population helpers.

use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

use super::populate_relationships_cached;
use crate::db::query::populate::{
    PopulateContext, PopulateCtx, PopulateOpts, document_to_json, locale_cache_key, parse_poly_ref,
    populate_cache_key,
};
use crate::{
    core::{Document, upload},
    db::query::read::{find_by_id, find_by_ids},
};

use crate::db::query::populate::helpers::{cache_get_doc, cache_set_doc};

/// Resolve a single polymorphic ref string to its populated value.
/// Returns the original string if resolution fails (missing collection, visited, etc.).
fn resolve_poly_item(
    ctx: &PopulateCtx<'_>,
    item: &str,
    fetched_map: &mut HashMap<String, HashMap<String, Document>>,
    visited: &mut HashSet<(String, String)>,
) -> Result<Value> {
    let (col, id) = match parse_poly_ref(item) {
        Some(pair) => pair,
        None => return Ok(Value::String(item.to_string())),
    };

    if visited.contains(&(col.clone(), id.clone())) {
        return Ok(Value::String(item.to_string()));
    }

    let col_map = match fetched_map.get_mut(&col) {
        Some(m) => m,
        None => return Ok(Value::String(item.to_string())),
    };

    let item_def = match ctx.registry.get_collection(&col) {
        Some(d) => d.clone(),
        None => return Ok(Value::String(item.to_string())),
    };

    let mut rd = match col_map.remove(&id) {
        Some(d) => d,
        None => return Ok(Value::String(item.to_string())),
    };

    if let Some(ref uc) = item_def.upload
        && uc.enabled
    {
        upload::assemble_sizes_object(&mut rd, uc);
    }

    populate_relationships_cached(
        &PopulateContext {
            conn: ctx.conn,
            registry: ctx.registry,
            collection_slug: &col,
            def: &item_def,
        },
        &mut rd,
        visited,
        &PopulateOpts {
            depth: ctx.effective_depth - 1,
            select: None,
            locale_ctx: ctx.locale_ctx,
        },
        ctx.cache,
    )?;

    let locale_key = locale_cache_key(ctx.locale_ctx);
    let key = populate_cache_key(&col, rd.id.as_ref(), locale_key.as_deref());
    let _ = cache_set_doc(ctx.cache, &key, &rd);

    Ok(document_to_json(&rd, &col))
}

/// Populate a polymorphic has-many field.
pub(super) fn populate_poly_has_many(
    ctx: &PopulateCtx<'_>,
    doc: &mut Document,
    field_name: &str,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let items: Vec<String> = match doc.fields.get(field_name) {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => return Ok(()),
    };

    // Group IDs by target collection for batch fetch
    let mut ids_by_collection: HashMap<String, Vec<String>> = HashMap::new();
    for item in &items {
        if let Some((col, id)) = parse_poly_ref(item)
            && !visited.contains(&(col.clone(), id.clone()))
        {
            ids_by_collection.entry(col).or_default().push(id);
        }
    }

    // Batch fetch per collection
    let mut fetched_map: HashMap<String, HashMap<String, Document>> = HashMap::new();
    for (col, col_ids) in &ids_by_collection {
        if let Some(item_def) = ctx.registry.get_collection(col) {
            let item_def = item_def.clone();
            let fetched = find_by_ids(ctx.conn, col, &item_def, col_ids, ctx.locale_ctx)?;
            let doc_map: HashMap<String, Document> =
                fetched.into_iter().map(|d| (d.id.to_string(), d)).collect();
            fetched_map.insert(col.clone(), doc_map);
        }
    }

    // Reassemble in original order
    let mut populated = Vec::new();
    for item in &items {
        let resolved = resolve_poly_item(ctx, item, &mut fetched_map, visited)?;
        populated.push(resolved);
    }

    doc.fields
        .insert(field_name.to_string(), Value::Array(populated));
    Ok(())
}

/// Populate a polymorphic has-one field.
pub(super) fn populate_poly_has_one(
    ctx: &PopulateCtx<'_>,
    doc: &mut Document,
    field_name: &str,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let raw = match doc.fields.get(field_name) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Ok(()),
    };

    let Some((col, id)) = parse_poly_ref(&raw) else {
        return Ok(());
    };

    if visited.contains(&(col.clone(), id.clone())) {
        return Ok(());
    }

    let Some(item_def) = ctx.registry.get_collection(&col) else {
        return Ok(());
    };

    let item_def = item_def.clone();
    let locale_key = locale_cache_key(ctx.locale_ctx);
    let key = populate_cache_key(&col, &id, locale_key.as_deref());

    if let Some(cached) = cache_get_doc(ctx.cache, &key)? {
        doc.fields
            .insert(field_name.to_string(), document_to_json(&cached, &col));
        return Ok(());
    }

    let Some(mut rd) = find_by_id(ctx.conn, &col, &item_def, &id, ctx.locale_ctx)? else {
        return Ok(());
    };

    if let Some(ref uc) = item_def.upload
        && uc.enabled
    {
        upload::assemble_sizes_object(&mut rd, uc);
    }

    populate_relationships_cached(
        &PopulateContext {
            conn: ctx.conn,
            registry: ctx.registry,
            collection_slug: &col,
            def: &item_def,
        },
        &mut rd,
        visited,
        &PopulateOpts {
            depth: ctx.effective_depth - 1,
            select: None,
            locale_ctx: ctx.locale_ctx,
        },
        ctx.cache,
    )?;

    let _ = cache_set_doc(ctx.cache, &key, &rd);
    doc.fields
        .insert(field_name.to_string(), document_to_json(&rd, &col));

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
    use crate::db::{DbConnection, query::join};
    use std::collections::HashSet;

    #[test]
    fn populate_polymorphic_has_one() {
        let conn = setup_polymorphic_populate_db();
        let entries_def = make_entries_def_poly_has_one();
        let articles_def =
            make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
        let pages_def = make_collection_def("pages", vec![make_field("title", FieldType::Text)]);
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());
        registry.register_collection(articles_def);
        registry.register_collection(pages_def);

        let mut doc = Document::new("e1".to_string());
        doc.fields.insert("title".to_string(), json!("Entry"));
        doc.fields
            .insert("related".to_string(), json!("articles/a1"));

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "entries",
                def: &entries_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &NoneCache,
        )
        .unwrap();

        // Should be populated as a full document object
        let related = doc.fields.get("related").expect("related should exist");
        assert!(
            related.is_object(),
            "polymorphic has-one should be populated to object"
        );
        assert_eq!(related.get("id").and_then(|v| v.as_str()), Some("a1"));
        assert_eq!(
            related.get("title").and_then(|v| v.as_str()),
            Some("Article One")
        );
        assert_eq!(
            related.get("collection").and_then(|v| v.as_str()),
            Some("articles")
        );
    }

    #[test]
    fn populate_polymorphic_has_one_depth_zero() {
        let conn = setup_polymorphic_populate_db();
        let entries_def = make_entries_def_poly_has_one();
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());

        let mut doc = Document::new("e1".to_string());
        doc.fields
            .insert("related".to_string(), json!("articles/a1"));

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "entries",
                def: &entries_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 0,
                select: None,
                locale_ctx: None,
            },
            &NoneCache,
        )
        .unwrap();

        // depth=0: should stay as composite string
        assert_eq!(
            doc.fields.get("related").and_then(|v| v.as_str()),
            Some("articles/a1")
        );
    }

    #[test]
    fn populate_polymorphic_has_many() {
        let conn = setup_polymorphic_populate_db();
        // Insert junction table data
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

        // Hydrate first (loads polymorphic has-many from junction table)
        let mut doc = Document::new("e1".to_string());
        doc.fields.insert("title".to_string(), json!("Entry"));
        join::hydrate_document(&conn, "entries", &entries_def.fields, &mut doc, None, None)
            .unwrap();

        // Verify hydration produced composite strings
        let refs = doc.fields.get("refs").expect("refs should be hydrated");
        let arr = refs.as_array().unwrap();
        assert_eq!(arr[0].as_str().unwrap(), "articles/a1");
        assert_eq!(arr[1].as_str().unwrap(), "pages/pg1");

        // Now populate
        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "entries",
                def: &entries_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &NoneCache,
        )
        .unwrap();

        let refs = doc
            .fields
            .get("refs")
            .expect("refs should exist after populate");
        let arr = refs.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        // First item: article
        assert!(arr[0].is_object(), "item should be populated object");
        assert_eq!(arr[0].get("id").and_then(|v| v.as_str()), Some("a1"));
        assert_eq!(
            arr[0].get("collection").and_then(|v| v.as_str()),
            Some("articles")
        );
        // Second item: page
        assert!(arr[1].is_object());
        assert_eq!(arr[1].get("id").and_then(|v| v.as_str()), Some("pg1"));
        assert_eq!(
            arr[1].get("collection").and_then(|v| v.as_str()),
            Some("pages")
        );
    }

    #[test]
    fn populate_polymorphic_unknown_collection_keeps_string() {
        let conn = setup_polymorphic_populate_db();
        let entries_def = make_entries_def_poly_has_one();
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());
        // Don't register "articles" — it's unknown

        let mut doc = Document::new("e1".to_string());
        doc.fields
            .insert("related".to_string(), json!("unknown_col/x1"));

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "entries",
                def: &entries_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &NoneCache,
        )
        .unwrap();

        // Unknown collection: value stays as string
        assert_eq!(
            doc.fields.get("related").and_then(|v| v.as_str()),
            Some("unknown_col/x1")
        );
    }

    #[test]
    fn populate_polymorphic_has_one_cache_hit() {
        let conn = setup_polymorphic_populate_db();
        let entries_def = make_entries_def_poly_has_one();
        let articles_def =
            make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
        let pages_def = make_collection_def("pages", vec![make_field("title", FieldType::Text)]);
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());
        registry.register_collection(articles_def);
        registry.register_collection(pages_def);

        // Pre-populate the cache with the article document
        let cache = MemoryCache::new(10_000);
        let mut cached_article = Document::new("a1".to_string());
        cached_article
            .fields
            .insert("title".to_string(), json!("Cached Article"));
        cache
            .set(
                &populate_cache_key("articles", "a1", None),
                &serde_json::to_vec(&cached_article).unwrap(),
            )
            .unwrap();

        let mut doc = Document::new("e1".to_string());
        doc.fields
            .insert("related".to_string(), json!("articles/a1"));

        let mut visited = HashSet::new();
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "entries",
                def: &entries_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &cache,
        )
        .unwrap();

        // Should use the cached document, not the DB version
        let related = doc.fields.get("related").expect("related should exist");
        assert!(related.is_object(), "should be populated from cache");
        assert_eq!(related.get("id").and_then(|v| v.as_str()), Some("a1"));
        // Cache returns "Cached Article", not "Article One" from DB
        assert_eq!(
            related.get("title").and_then(|v| v.as_str()),
            Some("Cached Article")
        );
    }

    #[test]
    fn populate_polymorphic_has_one_visited_stops() {
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

        // Mark a1 as already visited
        let mut visited = HashSet::new();
        visited.insert(("articles".to_string(), "a1".to_string()));

        let cache = NoneCache;
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "entries",
                def: &entries_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &cache,
        )
        .unwrap();

        // Should remain as string because it's visited
        assert_eq!(
            doc.fields.get("related").and_then(|v| v.as_str()),
            Some("articles/a1"),
            "visited poly has-one should not be populated"
        );
    }

    #[test]
    fn populate_polymorphic_has_many_malformed_item_keeps_as_string() {
        let conn = setup_polymorphic_populate_db();
        let entries_def = make_entries_def_poly_has_many();
        let articles_def =
            make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
        let pages_def = make_collection_def("pages", vec![make_field("title", FieldType::Text)]);
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());
        registry.register_collection(articles_def);
        registry.register_collection(pages_def);

        let mut doc = Document::new("e1".to_string());
        // Mix of valid composite strings and a malformed one (no slash)
        doc.fields.insert(
            "refs".to_string(),
            json!(["articles/a1", "malformed-no-slash"]),
        );

        let mut visited = HashSet::new();
        let cache = NoneCache;
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "entries",
                def: &entries_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &cache,
        )
        .unwrap();

        let refs = doc.fields.get("refs").expect("refs should exist");
        let arr = refs.as_array().expect("refs should be array");
        assert_eq!(arr.len(), 2);
        // First item: valid — should be populated
        assert!(arr[0].is_object(), "valid poly ref should be populated");
        assert_eq!(arr[0].get("id").and_then(|v| v.as_str()), Some("a1"));
        // Second item: malformed — should be kept as-is
        assert_eq!(
            arr[1].as_str(),
            Some("malformed-no-slash"),
            "malformed poly ref should remain as string"
        );
    }

    #[test]
    fn populate_polymorphic_has_many_visited_item_keeps_as_composite_string() {
        let conn = setup_polymorphic_populate_db();
        let entries_def = make_entries_def_poly_has_many();
        let articles_def =
            make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());
        registry.register_collection(articles_def);

        let mut doc = Document::new("e1".to_string());
        doc.fields
            .insert("refs".to_string(), json!(["articles/a1"]));

        // Mark a1 as visited before calling populate
        let mut visited = HashSet::new();
        visited.insert(("articles".to_string(), "a1".to_string()));

        let cache = NoneCache;
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "entries",
                def: &entries_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &cache,
        )
        .unwrap();

        let refs = doc.fields.get("refs").expect("refs should exist");
        let arr = refs.as_array().expect("refs should be array");
        assert_eq!(arr.len(), 1);
        // Visited item should remain as composite string
        assert_eq!(
            arr[0].as_str(),
            Some("articles/a1"),
            "visited poly ref should remain as composite string during reassembly"
        );
    }

    #[test]
    fn populate_polymorphic_has_many_unknown_collection_in_items_keeps_string() {
        let conn = setup_polymorphic_populate_db();
        let entries_def = make_entries_def_poly_has_many();
        // Only register "articles", not "unknown_col"
        let articles_def =
            make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());
        registry.register_collection(articles_def);

        let mut doc = Document::new("e1".to_string());
        // Mix: one valid, one with unknown collection (not in registry)
        doc.fields.insert(
            "refs".to_string(),
            json!(["articles/a1", "unknown_col/x99"]),
        );

        let mut visited = HashSet::new();
        let cache = NoneCache;
        populate_relationships_cached(
            &PopulateContext {
                conn: &conn,
                registry: &registry,
                collection_slug: "entries",
                def: &entries_def,
            },
            &mut doc,
            &mut visited,
            &PopulateOpts {
                depth: 1,
                select: None,
                locale_ctx: None,
            },
            &cache,
        )
        .unwrap();

        let refs = doc.fields.get("refs").expect("refs should exist");
        let arr = refs.as_array().expect("refs should be array");
        assert_eq!(arr.len(), 2);
        // Known collection: populated
        assert!(arr[0].is_object());
        // Unknown collection: keeps as composite string (fetched_map.get_mut returns None)
        assert_eq!(
            arr[1].as_str(),
            Some("unknown_col/x99"),
            "unknown collection in poly has-many should remain as string"
        );
    }
}
