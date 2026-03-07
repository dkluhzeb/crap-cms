//! Relationship population (depth-based recursive loading).

use anyhow::Result;
use std::collections::{HashMap, HashSet};
use dashmap::DashMap;

use crate::core::{CollectionDefinition, Document};
use crate::core::field::FieldType;
use super::read::{find, find_by_id, find_by_ids};
use super::{FindQuery, FilterClause, Filter, FilterOp, LocaleContext};

/// Shared cache for populated documents. Key is (collection_slug, document_id).
/// Uses DashMap for concurrent cross-request sharing with interior mutability.
pub type PopulateCache = DashMap<(String, String), Document>;

/// Parse a polymorphic reference "collection/id" into `(collection, id)`.
fn parse_poly_ref(s: &str) -> Option<(String, String)> {
    let pos = s.find('/')?;
    let col = &s[..pos];
    let id = &s[pos + 1..];
    if col.is_empty() || id.is_empty() { return None; }
    Some((col.to_string(), id.to_string()))
}

/// Convert a Document into a serde_json::Value for embedding in a parent's fields.
fn document_to_json(doc: &Document, collection: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("id".to_string(), serde_json::Value::String(doc.id.clone()));
    map.insert("collection".to_string(), serde_json::Value::String(collection.to_string()));
    for (k, v) in &doc.fields {
        map.insert(k.clone(), v.clone());
    }
    if let Some(ref ts) = doc.created_at {
        map.insert("created_at".to_string(), serde_json::Value::String(ts.clone()));
    }
    if let Some(ref ts) = doc.updated_at {
        map.insert("updated_at".to_string(), serde_json::Value::String(ts.clone()));
    }
    serde_json::Value::Object(map)
}

/// Recursively populate relationship fields with full document objects.
/// Convenience wrapper that creates a fresh cache per call.
#[allow(clippy::too_many_arguments)]
pub fn populate_relationships(
    conn: &rusqlite::Connection,
    registry: &crate::core::Registry,
    collection_slug: &str,
    def: &CollectionDefinition,
    doc: &mut Document,
    depth: i32,
    visited: &mut HashSet<(String, String)>,
    select: Option<&[String]>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    let cache = PopulateCache::new();
    populate_relationships_cached(conn, registry, collection_slug, def, doc, depth, visited, select, &cache, locale_ctx)
}

/// Recursively populate relationship fields with full document objects.
/// depth=0 is a no-op. Tracks visited (collection, id) pairs to break cycles.
/// If `select` is provided, only populate relationship fields in the select list.
/// Uses a shared `cache` to avoid redundant fetches within the same request.
#[allow(clippy::too_many_arguments)]
pub fn populate_relationships_cached(
    conn: &rusqlite::Connection,
    registry: &crate::core::Registry,
    collection_slug: &str,
    def: &CollectionDefinition,
    doc: &mut Document,
    depth: i32,
    visited: &mut HashSet<(String, String)>,
    select: Option<&[String]>,
    cache: &PopulateCache,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    if depth <= 0 {
        return Ok(());
    }

    let visit_key = (collection_slug.to_string(), doc.id.clone());
    if visited.contains(&visit_key) {
        return Ok(());
    }
    visited.insert(visit_key);

    for field in &def.fields {
        if field.field_type != FieldType::Relationship && field.field_type != FieldType::Upload {
            continue;
        }
        // Skip populating fields not in the select list
        if let Some(sel) = select {
            if !sel.iter().any(|s| s == &field.name) {
                continue;
            }
        }
        let rel = match &field.relationship {
            Some(rc) => rc,
            None => continue,
        };

        // Field-level max_depth caps the effective depth for this field
        let effective_depth = match rel.max_depth {
            Some(max) if max < depth => max,
            _ => depth,
        };
        if effective_depth <= 0 {
            continue;
        }

        if rel.is_polymorphic() {
            // Polymorphic: values are "collection/id" composite strings
            if rel.has_many {
                let items: Vec<String> = match doc.fields.get(&field.name) {
                    Some(serde_json::Value::Array(arr)) => {
                        arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                    }
                    _ => continue,
                };

                // Group IDs by target collection for batch fetch
                let mut ids_by_collection: HashMap<String, Vec<String>> = HashMap::new();
                for item in &items {
                    if let Some((col, id)) = parse_poly_ref(item) {
                        if !visited.contains(&(col.clone(), id.clone())) {
                            ids_by_collection.entry(col).or_default().push(id);
                        }
                    }
                }

                // Batch fetch per collection
                let mut fetched_map: HashMap<String, HashMap<String, Document>> = HashMap::new();
                for (col, col_ids) in &ids_by_collection {
                    if let Some(item_def) = registry.get_collection(col) {
                        let item_def = item_def.clone();
                        let fetched = find_by_ids(conn, col, &item_def, col_ids, locale_ctx)?;
                        let doc_map: HashMap<String, Document> = fetched.into_iter()
                            .map(|d| (d.id.clone(), d))
                            .collect();
                        fetched_map.insert(col.clone(), doc_map);
                    }
                }

                // Reassemble in original order
                let mut populated = Vec::new();
                for item in &items {
                    if let Some((col, id)) = parse_poly_ref(item) {
                        if visited.contains(&(col.clone(), id.clone())) {
                            populated.push(serde_json::Value::String(item.clone()));
                            continue;
                        }
                        if let Some(col_map) = fetched_map.get_mut(&col) {
                            if let Some(item_def) = registry.get_collection(&col) {
                                let item_def = item_def.clone();
                                if let Some(mut rd) = col_map.remove(&id) {
                                    if let Some(ref uc) = item_def.upload {
                                        if uc.enabled { crate::core::upload::assemble_sizes_object(&mut rd, uc); }
                                    }
                                    populate_relationships_cached(conn, registry, &col, &item_def, &mut rd, effective_depth - 1, visited, None, cache, locale_ctx)?;
                                    cache.insert((col.clone(), rd.id.clone()), rd.clone());
                                    populated.push(document_to_json(&rd, &col));
                                } else {
                                    populated.push(serde_json::Value::String(item.clone()));
                                }
                            } else {
                                populated.push(serde_json::Value::String(item.clone()));
                            }
                        } else {
                            populated.push(serde_json::Value::String(item.clone()));
                        }
                    } else {
                        populated.push(serde_json::Value::String(item.clone()));
                    }
                }
                doc.fields.insert(field.name.clone(), serde_json::Value::Array(populated));
            } else {
                // Polymorphic has-one: stored as "collection/id"
                let raw = match doc.fields.get(&field.name) {
                    Some(serde_json::Value::String(s)) if !s.is_empty() => s.clone(),
                    _ => continue,
                };
                if let Some((col, id)) = parse_poly_ref(&raw) {
                    if visited.contains(&(col.clone(), id.clone())) { continue; }
                    if let Some(item_def) = registry.get_collection(&col) {
                        let item_def = item_def.clone();
                        let poly_cache_key = (col.clone(), id.clone());
                        if let Some(cached) = cache.get(&poly_cache_key) {
                            doc.fields.insert(field.name.clone(), document_to_json(cached.value(), &col));
                        } else if let Some(mut rd) = find_by_id(conn, &col, &item_def, &id, locale_ctx)? {
                            if let Some(ref uc) = item_def.upload {
                                if uc.enabled { crate::core::upload::assemble_sizes_object(&mut rd, uc); }
                            }
                            populate_relationships_cached(conn, registry, &col, &item_def, &mut rd, effective_depth - 1, visited, None, cache, locale_ctx)?;
                            cache.insert(poly_cache_key, rd.clone());
                            doc.fields.insert(field.name.clone(), document_to_json(&rd, &col));
                        }
                    }
                }
            }
        } else {
            // Non-polymorphic: look up the target collection definition
            let rel_def = match registry.get_collection(&rel.collection) {
                Some(d) => d.clone(),
                None => continue,
            };

            if rel.has_many {
                // Has-many: doc.fields[name] is already a JSON array of ID strings (from hydration)
                let ids: Vec<String> = match doc.fields.get(&field.name) {
                    Some(serde_json::Value::Array(arr)) => {
                        arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                    }
                    _ => continue,
                };

                // Collect IDs not yet visited for batch fetch
                let fetch_ids: Vec<String> = ids.iter()
                    .filter(|id| !visited.contains(&(rel.collection.clone(), id.to_string())))
                    .cloned()
                    .collect();

                // Batch fetch all needed documents in one query
                let fetched = find_by_ids(conn, &rel.collection, &rel_def, &fetch_ids, locale_ctx)?;
                let mut fetched_map: HashMap<String, Document> = fetched.into_iter()
                    .map(|d| (d.id.clone(), d))
                    .collect();

                // Reassemble in original order
                let mut populated = Vec::new();
                for id in &ids {
                    if visited.contains(&(rel.collection.clone(), id.clone())) {
                        populated.push(serde_json::Value::String(id.clone()));
                        continue;
                    }
                    let hm_cache_key = (rel.collection.clone(), id.clone());
                    if let Some(cached) = cache.get(&hm_cache_key) {
                        populated.push(document_to_json(cached.value(), &rel.collection));
                    } else {
                        match fetched_map.remove(id) {
                            Some(mut related_doc) => {
                                if let Some(ref uc) = rel_def.upload {
                                    if uc.enabled {
                                        crate::core::upload::assemble_sizes_object(&mut related_doc, uc);
                                    }
                                }
                                populate_relationships_cached(
                                    conn, registry, &rel.collection, &rel_def,
                                    &mut related_doc, effective_depth - 1, visited, None, cache, locale_ctx,
                                )?;
                                cache.insert(hm_cache_key, related_doc.clone());
                                populated.push(document_to_json(&related_doc, &rel.collection));
                            }
                            None => {
                                populated.push(serde_json::Value::String(id.clone()));
                            }
                        }
                    }
                }
                doc.fields.insert(field.name.clone(), serde_json::Value::Array(populated));
            } else {
                // Has-one: doc.fields[name] is a string ID
                let id = match doc.fields.get(&field.name) {
                    Some(serde_json::Value::String(s)) if !s.is_empty() => s.clone(),
                    _ => continue,
                };

                if visited.contains(&(rel.collection.clone(), id.clone())) {
                    continue;
                }

                let ho_cache_key = (rel.collection.clone(), id.clone());
                if let Some(cached) = cache.get(&ho_cache_key) {
                    doc.fields.insert(field.name.clone(), document_to_json(cached.value(), &rel.collection));
                } else if let Some(mut related_doc) = find_by_id(conn, &rel.collection, &rel_def, &id, locale_ctx)? {
                    if let Some(ref uc) = rel_def.upload {
                        if uc.enabled {
                            crate::core::upload::assemble_sizes_object(&mut related_doc, uc);
                        }
                    }
                    populate_relationships_cached(
                        conn, registry, &rel.collection, &rel_def,
                        &mut related_doc, effective_depth - 1, visited, None, cache, locale_ctx,
                    )?;
                    cache.insert(ho_cache_key, related_doc.clone());
                    doc.fields.insert(field.name.clone(), document_to_json(&related_doc, &rel.collection));
                }
            }
        }
    }

    // Join fields: virtual reverse lookups
    for field in &def.fields {
        if field.field_type != FieldType::Join {
            continue;
        }
        if let Some(sel) = select {
            if !sel.iter().any(|s| s == &field.name) {
                continue;
            }
        }
        let jc = match &field.join {
            Some(jc) => jc,
            None => continue,
        };
        let target_def = match registry.get_collection(&jc.collection) {
            Some(d) => d.clone(),
            None => continue,
        };
        let fq = FindQuery {
            filters: vec![FilterClause::Single(Filter {
                field: jc.on.clone(),
                op: FilterOp::Equals(doc.id.clone()),
            })],
            ..Default::default()
        };
        if let Ok(matched_docs) = find(conn, &jc.collection, &target_def, &fq, locale_ctx) {
            let mut populated = Vec::new();
            for mut matched_doc in matched_docs {
                super::hydrate_document(conn, &jc.collection, &target_def.fields, &mut matched_doc, None, locale_ctx)?;
                if let Some(ref uc) = target_def.upload {
                    if uc.enabled {
                        crate::core::upload::assemble_sizes_object(&mut matched_doc, uc);
                    }
                }
                populate_relationships_cached(
                    conn, registry, &jc.collection, &target_def,
                    &mut matched_doc, depth - 1, visited, None, cache, locale_ctx,
                )?;
                populated.push(document_to_json(&matched_doc, &jc.collection));
            }
            doc.fields.insert(field.name.clone(), serde_json::Value::Array(populated));
        }
    }

    Ok(())
}

/// Batch-populate relationship fields across a slice of documents.
/// Convenience wrapper that creates a fresh cache per call.
#[allow(clippy::too_many_arguments)]
pub fn populate_relationships_batch(
    conn: &rusqlite::Connection,
    registry: &crate::core::Registry,
    collection_slug: &str,
    def: &CollectionDefinition,
    docs: &mut [Document],
    depth: i32,
    select: Option<&[String]>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    let cache = PopulateCache::new();
    populate_relationships_batch_cached(conn, registry, collection_slug, def, docs, depth, select, &cache, locale_ctx)
}

/// Batch-populate relationship fields across a slice of documents.
///
/// Instead of calling `populate_relationships` per-document (N×M individual queries),
/// this collects all referenced IDs across all documents per field, batch-fetches them
/// with a single `find_by_ids` query per target collection, then distributes the results
/// back. Uses a shared `cache` to avoid redundant fetches within the same request.
///
/// This is the hot path for `Find` with `depth >= 1` — it turns O(N×M) queries into
/// O(M) queries where M is the number of relationship fields.
#[allow(clippy::too_many_arguments)]
pub fn populate_relationships_batch_cached(
    conn: &rusqlite::Connection,
    registry: &crate::core::Registry,
    collection_slug: &str,
    def: &CollectionDefinition,
    docs: &mut [Document],
    depth: i32,
    select: Option<&[String]>,
    cache: &PopulateCache,
    locale_ctx: Option<&LocaleContext>,
) -> Result<()> {
    if depth <= 0 || docs.is_empty() {
        return Ok(());
    }

    // Shared visited set across all documents for cross-document dedup
    let mut visited: HashSet<(String, String)> = HashSet::new();
    // Mark all parent documents as visited to prevent circular population
    for doc in docs.iter() {
        visited.insert((collection_slug.to_string(), doc.id.clone()));
    }

    // ── Non-join relationship/upload fields ──────────────────────────────
    for field in &def.fields {
        if field.field_type != FieldType::Relationship && field.field_type != FieldType::Upload {
            continue;
        }
        if let Some(sel) = select {
            if !sel.iter().any(|s| s == &field.name) {
                continue;
            }
        }
        let rel = match &field.relationship {
            Some(rc) => rc,
            None => continue,
        };

        let effective_depth = match rel.max_depth {
            Some(max) if max < depth => max,
            _ => depth,
        };
        if effective_depth <= 0 {
            continue;
        }

        if rel.is_polymorphic() {
            // ── Polymorphic fields: group by target collection ───────────
            if rel.has_many {
                // Collect all unique poly refs across all docs
                let mut ids_by_collection: HashMap<String, Vec<String>> = HashMap::new();
                for doc in docs.iter() {
                    if let Some(serde_json::Value::Array(arr)) = doc.fields.get(&field.name) {
                        for v in arr {
                            if let Some(s) = v.as_str() {
                                if let Some((col, id)) = parse_poly_ref(s) {
                                    if !visited.contains(&(col.clone(), id.clone())) {
                                        ids_by_collection.entry(col).or_default().push(id);
                                    }
                                }
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
                let mut fetched_map: HashMap<String, HashMap<String, Document>> = HashMap::new();
                for (col, col_ids) in &ids_by_collection {
                    if let Some(item_def) = registry.get_collection(col) {
                        let item_def = item_def.clone();
                        // Check cache for already-populated docs
                        let mut doc_map: HashMap<String, Document> = HashMap::new();
                        let mut uncached_ids: Vec<String> = Vec::new();
                        for id in col_ids {
                            let key = (col.clone(), id.clone());
                            if let Some(cached) = cache.get(&key) {
                                doc_map.insert(id.clone(), cached.value().clone());
                            } else {
                                uncached_ids.push(id.clone());
                            }
                        }
                        if !uncached_ids.is_empty() {
                            let mut fetched = find_by_ids(conn, col, &item_def, &uncached_ids, locale_ctx)?;
                            for d in &mut fetched {
                                if let Some(ref uc) = item_def.upload {
                                    if uc.enabled {
                                        crate::core::upload::assemble_sizes_object(d, uc);
                                    }
                                }
                            }
                            if effective_depth - 1 > 0 {
                                populate_relationships_batch_cached(
                                    conn, registry, col, &item_def,
                                    &mut fetched, effective_depth - 1, None, cache, locale_ctx,
                                )?;
                            }
                            for d in fetched {
                                cache.insert((col.clone(), d.id.clone()), d.clone());
                                doc_map.insert(d.id.clone(), d);
                            }
                        }
                        fetched_map.insert(col.clone(), doc_map);
                    }
                }

                // Distribute results back to each document
                for doc in docs.iter_mut() {
                    let items: Vec<String> = match doc.fields.get(&field.name) {
                        Some(serde_json::Value::Array(arr)) => {
                            arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                        }
                        _ => continue,
                    };
                    let mut populated = Vec::new();
                    for item in &items {
                        if let Some((col, id)) = parse_poly_ref(item) {
                            if let Some(col_map) = fetched_map.get(&col) {
                                if let Some(cached_doc) = col_map.get(&id) {
                                    populated.push(document_to_json(cached_doc, &col));
                                } else {
                                    populated.push(serde_json::Value::String(item.clone()));
                                }
                            } else {
                                populated.push(serde_json::Value::String(item.clone()));
                            }
                        } else {
                            populated.push(serde_json::Value::String(item.clone()));
                        }
                    }
                    doc.fields.insert(field.name.clone(), serde_json::Value::Array(populated));
                }
            } else {
                // Polymorphic has-one: group by target collection
                let mut ids_by_collection: HashMap<String, Vec<String>> = HashMap::new();
                for doc in docs.iter() {
                    if let Some(serde_json::Value::String(s)) = doc.fields.get(&field.name) {
                        if !s.is_empty() {
                            if let Some((col, id)) = parse_poly_ref(s) {
                                if !visited.contains(&(col.clone(), id.clone())) {
                                    ids_by_collection.entry(col).or_default().push(id);
                                }
                            }
                        }
                    }
                }

                for ids in ids_by_collection.values_mut() {
                    ids.sort();
                    ids.dedup();
                }

                let mut fetched_map: HashMap<String, HashMap<String, Document>> = HashMap::new();
                for (col, col_ids) in &ids_by_collection {
                    if let Some(item_def) = registry.get_collection(col) {
                        let item_def = item_def.clone();
                        let mut doc_map: HashMap<String, Document> = HashMap::new();
                        let mut uncached_ids: Vec<String> = Vec::new();
                        for id in col_ids {
                            let key = (col.clone(), id.clone());
                            if let Some(cached) = cache.get(&key) {
                                doc_map.insert(id.clone(), cached.value().clone());
                            } else {
                                uncached_ids.push(id.clone());
                            }
                        }
                        if !uncached_ids.is_empty() {
                            let mut fetched = find_by_ids(conn, col, &item_def, &uncached_ids, locale_ctx)?;
                            for d in &mut fetched {
                                if let Some(ref uc) = item_def.upload {
                                    if uc.enabled {
                                        crate::core::upload::assemble_sizes_object(d, uc);
                                    }
                                }
                            }
                            if effective_depth - 1 > 0 {
                                populate_relationships_batch_cached(
                                    conn, registry, col, &item_def,
                                    &mut fetched, effective_depth - 1, None, cache, locale_ctx,
                                )?;
                            }
                            for d in fetched {
                                cache.insert((col.clone(), d.id.clone()), d.clone());
                                doc_map.insert(d.id.clone(), d);
                            }
                        }
                        fetched_map.insert(col.clone(), doc_map);
                    }
                }

                for doc in docs.iter_mut() {
                    let raw = match doc.fields.get(&field.name) {
                        Some(serde_json::Value::String(s)) if !s.is_empty() => s.clone(),
                        _ => continue,
                    };
                    if let Some((col, id)) = parse_poly_ref(&raw) {
                        if let Some(col_map) = fetched_map.get(&col) {
                            if let Some(cached_doc) = col_map.get(&id) {
                                doc.fields.insert(field.name.clone(), document_to_json(cached_doc, &col));
                            }
                        }
                    }
                }
            }
        } else {
            // ── Non-polymorphic relationship ─────────────────────────────
            let rel_def = match registry.get_collection(&rel.collection) {
                Some(d) => d.clone(),
                None => continue,
            };

            if rel.has_many {
                // Collect all unique IDs across all docs for this has-many field
                let mut all_ids: Vec<String> = Vec::new();
                for doc in docs.iter() {
                    if let Some(serde_json::Value::Array(arr)) = doc.fields.get(&field.name) {
                        for v in arr {
                            if let Some(s) = v.as_str() {
                                if !visited.contains(&(rel.collection.clone(), s.to_string())) {
                                    all_ids.push(s.to_string());
                                }
                            }
                        }
                    }
                }
                all_ids.sort();
                all_ids.dedup();

                // Check cache, only fetch uncached IDs
                let mut doc_map: HashMap<String, Document> = HashMap::new();
                let mut uncached_ids: Vec<String> = Vec::new();
                for id in &all_ids {
                    let key = (rel.collection.clone(), id.clone());
                    if let Some(cached) = cache.get(&key) {
                        doc_map.insert(id.clone(), cached.value().clone());
                    } else {
                        uncached_ids.push(id.clone());
                    }
                }
                if !uncached_ids.is_empty() {
                    let mut fetched = find_by_ids(conn, &rel.collection, &rel_def, &uncached_ids, locale_ctx)?;
                    for d in &mut fetched {
                        if let Some(ref uc) = rel_def.upload {
                            if uc.enabled {
                                crate::core::upload::assemble_sizes_object(d, uc);
                            }
                        }
                    }
                    if effective_depth - 1 > 0 {
                        populate_relationships_batch_cached(
                            conn, registry, &rel.collection, &rel_def,
                            &mut fetched, effective_depth - 1, None, cache, locale_ctx,
                        )?;
                    }
                    for d in fetched {
                        cache.insert((rel.collection.clone(), d.id.clone()), d.clone());
                        doc_map.insert(d.id.clone(), d);
                    }
                }

                // Distribute back to each document preserving order
                for doc in docs.iter_mut() {
                    let ids: Vec<String> = match doc.fields.get(&field.name) {
                        Some(serde_json::Value::Array(arr)) => {
                            arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                        }
                        _ => continue,
                    };
                    let mut populated = Vec::new();
                    for id in &ids {
                        if let Some(cached_doc) = doc_map.get(id) {
                            populated.push(document_to_json(cached_doc, &rel.collection));
                        } else {
                            populated.push(serde_json::Value::String(id.clone()));
                        }
                    }
                    doc.fields.insert(field.name.clone(), serde_json::Value::Array(populated));
                }
            } else {
                // Has-one: collect all unique IDs across all docs
                let mut all_ids: Vec<String> = Vec::new();
                for doc in docs.iter() {
                    if let Some(serde_json::Value::String(s)) = doc.fields.get(&field.name) {
                        if !s.is_empty() && !visited.contains(&(rel.collection.clone(), s.clone())) {
                            all_ids.push(s.clone());
                        }
                    }
                }
                all_ids.sort();
                all_ids.dedup();

                // Check cache, only fetch uncached IDs
                let mut doc_map: HashMap<String, Document> = HashMap::new();
                let mut uncached_ids: Vec<String> = Vec::new();
                for id in &all_ids {
                    let key = (rel.collection.clone(), id.clone());
                    if let Some(cached) = cache.get(&key) {
                        doc_map.insert(id.clone(), cached.value().clone());
                    } else {
                        uncached_ids.push(id.clone());
                    }
                }
                if !uncached_ids.is_empty() {
                    let mut fetched = find_by_ids(conn, &rel.collection, &rel_def, &uncached_ids, locale_ctx)?;
                    for d in &mut fetched {
                        if let Some(ref uc) = rel_def.upload {
                            if uc.enabled {
                                crate::core::upload::assemble_sizes_object(d, uc);
                            }
                        }
                    }
                    if effective_depth - 1 > 0 {
                        populate_relationships_batch_cached(
                            conn, registry, &rel.collection, &rel_def,
                            &mut fetched, effective_depth - 1, None, cache, locale_ctx,
                        )?;
                    }
                    for d in fetched {
                        cache.insert((rel.collection.clone(), d.id.clone()), d.clone());
                        doc_map.insert(d.id.clone(), d);
                    }
                }

                // Distribute back
                for doc in docs.iter_mut() {
                    let id = match doc.fields.get(&field.name) {
                        Some(serde_json::Value::String(s)) if !s.is_empty() => s.clone(),
                        _ => continue,
                    };
                    if let Some(cached_doc) = doc_map.get(&id) {
                        doc.fields.insert(field.name.clone(), document_to_json(cached_doc, &rel.collection));
                    }
                }
            }
        }
    }

    // ── Join fields: fall through to per-doc (reverse lookups can't batch easily) ──
    let has_join_fields = def.fields.iter().any(|f| {
        f.field_type == FieldType::Join
            && f.join.is_some()
            && select.map_or(true, |sel| sel.iter().any(|s| s == &f.name))
    });
    if has_join_fields {
        for doc in docs.iter_mut() {
            let mut doc_visited = visited.clone();
            // Only run per-doc populate for join fields — relationship fields are already done
            for field in &def.fields {
                if field.field_type != FieldType::Join {
                    continue;
                }
                if let Some(sel) = select {
                    if !sel.iter().any(|s| s == &field.name) {
                        continue;
                    }
                }
                let jc = match &field.join {
                    Some(jc) => jc,
                    None => continue,
                };
                let target_def = match registry.get_collection(&jc.collection) {
                    Some(d) => d.clone(),
                    None => continue,
                };
                let fq = FindQuery {
                    filters: vec![FilterClause::Single(Filter {
                        field: jc.on.clone(),
                        op: FilterOp::Equals(doc.id.clone()),
                    })],
                    ..Default::default()
                };
                if let Ok(matched_docs) = find(conn, &jc.collection, &target_def, &fq, locale_ctx) {
                    let mut populated = Vec::new();
                    for mut matched_doc in matched_docs {
                        super::hydrate_document(conn, &jc.collection, &target_def.fields, &mut matched_doc, None, locale_ctx)?;
                        if let Some(ref uc) = target_def.upload {
                            if uc.enabled {
                                crate::core::upload::assemble_sizes_object(&mut matched_doc, uc);
                            }
                        }
                        populate_relationships_cached(
                            conn, registry, &jc.collection, &target_def,
                            &mut matched_doc, depth - 1, &mut doc_visited, None, cache, locale_ctx,
                        )?;
                        populated.push(document_to_json(&matched_doc, &jc.collection));
                    }
                    doc.fields.insert(field.name.clone(), serde_json::Value::Array(populated));
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use crate::core::{Document, Registry};
    use crate::core::collection::*;
    use crate::core::field::*;

    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: ft,
            ..Default::default()
        }
    }

    fn make_collection_def(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
        CollectionDefinition {
            slug: slug.to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields,
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            mcp: Default::default(),
            live: None,
            versions: None,
            indexes: Vec::new(),
        }
    }

    // ── document_to_json tests ────────────────────────────────────────────────

    #[test]
    fn document_to_json_basic() {
        let mut doc = Document::new("doc1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Hello World"));
        doc.fields.insert("count".to_string(), serde_json::json!(42));
        doc.created_at = Some("2024-01-01T00:00:00Z".to_string());
        doc.updated_at = Some("2024-01-02T00:00:00Z".to_string());

        let json = document_to_json(&doc, "posts");
        let obj = json.as_object().expect("should be an object");

        assert_eq!(obj.get("id").and_then(|v| v.as_str()), Some("doc1"));
        assert_eq!(obj.get("collection").and_then(|v| v.as_str()), Some("posts"));
        assert_eq!(obj.get("title").and_then(|v| v.as_str()), Some("Hello World"));
        assert_eq!(obj.get("count").and_then(|v| v.as_i64()), Some(42));
        assert_eq!(obj.get("created_at").and_then(|v| v.as_str()), Some("2024-01-01T00:00:00Z"));
        assert_eq!(obj.get("updated_at").and_then(|v| v.as_str()), Some("2024-01-02T00:00:00Z"));
    }

    #[test]
    fn document_to_json_no_timestamps() {
        let mut doc = Document::new("doc2".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("No Timestamps"));
        // created_at and updated_at are None by default

        let json = document_to_json(&doc, "pages");
        let obj = json.as_object().expect("should be an object");

        assert_eq!(obj.get("id").and_then(|v| v.as_str()), Some("doc2"));
        assert_eq!(obj.get("collection").and_then(|v| v.as_str()), Some("pages"));
        assert_eq!(obj.get("title").and_then(|v| v.as_str()), Some("No Timestamps"));
        assert!(obj.get("created_at").is_none(), "created_at should be absent");
        assert!(obj.get("updated_at").is_none(), "updated_at should be absent");
    }

    #[test]
    fn document_to_json_with_nested() {
        let mut doc = Document::new("doc3".to_string());
        let nested = serde_json::json!({
            "meta": {
                "keywords": ["rust", "cms"],
                "score": 9.5
            }
        });
        doc.fields.insert("data".to_string(), nested.clone());

        let json = document_to_json(&doc, "entries");
        let obj = json.as_object().expect("should be an object");

        assert_eq!(obj.get("data"), Some(&nested));
        // Verify deep structure is preserved
        let data = obj.get("data").unwrap();
        let meta = data.get("meta").expect("meta should exist");
        assert_eq!(meta.get("score").and_then(|v| v.as_f64()), Some(9.5));
        let keywords = meta.get("keywords").and_then(|v| v.as_array()).expect("keywords should be array");
        assert_eq!(keywords.len(), 2);
        assert_eq!(keywords[0].as_str(), Some("rust"));
    }

    // ── populate_relationships tests ──────────────────────────────────────────

    fn setup_populate_db() -> Connection {
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
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO authors (id, name, created_at, updated_at)
                VALUES ('a1', 'Alice', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, author, created_at, updated_at)
                VALUES ('p1', 'Hello', 'a1', '2024-01-01', '2024-01-01');"
        ).unwrap();
        conn
    }

    fn make_authors_def() -> CollectionDefinition {
        make_collection_def("authors", vec![
            make_field("name", FieldType::Text),
        ])
    }

    fn make_posts_def() -> CollectionDefinition {
        let mut author_field = make_field("author", FieldType::Relationship);
        author_field.relationship = Some(RelationshipConfig {
            collection: "authors".to_string(),
            has_many: false,
            max_depth: None,
            polymorphic: vec![],
        });
        make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            author_field,
        ])
    }

    fn make_registry_with_posts_and_authors() -> Registry {
        let mut registry = Registry::new();
        registry.register_collection(make_posts_def());
        registry.register_collection(make_authors_def());
        registry
    }

    #[test]
    fn populate_depth_zero_noop() {
        let conn = setup_populate_db();
        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Hello"));
        doc.fields.insert("author".to_string(), serde_json::json!("a1"));
        doc.created_at = Some("2024-01-01".to_string());
        doc.updated_at = Some("2024-01-01".to_string());

        let mut visited = HashSet::new();
        populate_relationships(
            &conn, &registry, "posts", &posts_def,
            &mut doc, 0, &mut visited, None, None).unwrap();

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
        doc.fields.insert("title".to_string(), serde_json::json!("Hello"));
        doc.fields.insert("author".to_string(), serde_json::json!("a1"));
        doc.created_at = Some("2024-01-01".to_string());
        doc.updated_at = Some("2024-01-01".to_string());

        let mut visited = HashSet::new();
        populate_relationships(
            &conn, &registry, "posts", &posts_def,
            &mut doc, 1, &mut visited, None, None).unwrap();

        // Author field should now be a populated object
        let author = doc.fields.get("author").expect("author field should exist");
        assert!(author.is_object(), "author should be populated as an object, got {:?}", author);

        let author_obj = author.as_object().unwrap();
        assert_eq!(author_obj.get("id").and_then(|v| v.as_str()), Some("a1"));
        assert_eq!(author_obj.get("name").and_then(|v| v.as_str()), Some("Alice"));
        assert_eq!(author_obj.get("collection").and_then(|v| v.as_str()), Some("authors"));
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
                VALUES ('p1', 'Hello', 'a1', '2024-01-01', '2024-01-01');"
        ).unwrap();

        // Authors def with a relationship back to posts
        let mut fav_post_field = make_field("favorite_post", FieldType::Relationship);
        fav_post_field.relationship = Some(RelationshipConfig {
            collection: "posts".to_string(),
            has_many: false,
            max_depth: None,
            polymorphic: vec![],
        });
        let authors_def = make_collection_def("authors", vec![
            make_field("name", FieldType::Text),
            fav_post_field,
        ]);

        // Posts def with relationship to authors
        let posts_def = make_posts_def();

        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(authors_def);

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Hello"));
        doc.fields.insert("author".to_string(), serde_json::json!("a1"));
        doc.created_at = Some("2024-01-01".to_string());
        doc.updated_at = Some("2024-01-01".to_string());

        // Pre-populate visited with the post itself to simulate already being in the chain
        let mut visited = HashSet::new();

        // Use high depth to ensure circular ref protection kicks in rather than depth limit
        let result = populate_relationships(
            &conn, &registry, "posts", &posts_def,
            &mut doc, 10, &mut visited, None, None,
        );

        assert!(result.is_ok(), "should not infinite loop on circular references");

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
                VALUES ('p1', 'c2', 1);"
        ).unwrap();

        let cats_def = make_collection_def("categories", vec![
            make_field("name", FieldType::Text),
        ]);

        let mut tags_field = make_field("tags", FieldType::Relationship);
        tags_field.relationship = Some(RelationshipConfig {
            collection: "categories".to_string(),
            has_many: true,
            max_depth: None,
            polymorphic: vec![],
        });
        let posts_def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            tags_field,
        ]);

        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(cats_def);

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Hello"));
        doc.fields.insert("tags".to_string(), serde_json::json!(["c1", "c2"]));
        doc.created_at = Some("2024-01-01".to_string());
        doc.updated_at = Some("2024-01-01".to_string());

        let mut visited = HashSet::new();
        populate_relationships(
            &conn, &registry, "posts", &posts_def,
            &mut doc, 1, &mut visited, None, None).unwrap();

        let tags = doc.fields.get("tags").expect("tags field should exist");
        let arr = tags.as_array().expect("tags should be an array");
        assert_eq!(arr.len(), 2);
        assert!(arr[0].is_object(), "first tag should be populated as object");
        assert_eq!(arr[0].get("name").and_then(|v| v.as_str()), Some("Tech"));
        assert!(arr[1].is_object(), "second tag should be populated as object");
        assert_eq!(arr[1].get("name").and_then(|v| v.as_str()), Some("Science"));
    }

    #[test]
    fn populate_has_many_missing_related_keeps_id() {
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
            INSERT INTO posts (id, title, created_at, updated_at)
                VALUES ('p1', 'Hello', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let cats_def = make_collection_def("categories", vec![
            make_field("name", FieldType::Text),
        ]);

        let mut tags_field = make_field("tags", FieldType::Relationship);
        tags_field.relationship = Some(RelationshipConfig {
            collection: "categories".to_string(),
            has_many: true,
            max_depth: None,
            polymorphic: vec![],
        });
        let posts_def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            tags_field,
        ]);

        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(cats_def);

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Hello"));
        // Reference IDs that don't exist in categories table
        doc.fields.insert("tags".to_string(), serde_json::json!(["nonexistent1", "nonexistent2"]));

        let mut visited = HashSet::new();
        populate_relationships(
            &conn, &registry, "posts", &posts_def,
            &mut doc, 1, &mut visited, None, None).unwrap();

        let tags = doc.fields.get("tags").expect("tags should exist");
        let arr = tags.as_array().expect("tags should be an array");
        assert_eq!(arr.len(), 2);
        // Missing related docs should remain as string IDs
        assert_eq!(arr[0].as_str(), Some("nonexistent1"));
        assert_eq!(arr[1].as_str(), Some("nonexistent2"));
    }

    #[test]
    fn populate_field_level_max_depth_caps() {
        let conn = setup_populate_db();

        // Create a field with max_depth = 0 — should not populate even at depth=1
        let mut author_field = make_field("author", FieldType::Relationship);
        author_field.relationship = Some(RelationshipConfig {
            collection: "authors".to_string(),
            has_many: false,
            max_depth: Some(0),
            polymorphic: vec![],
        });
        let posts_def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            author_field,
        ]);

        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(make_authors_def());

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Hello"));
        doc.fields.insert("author".to_string(), serde_json::json!("a1"));

        let mut visited = HashSet::new();
        populate_relationships(
            &conn, &registry, "posts", &posts_def,
            &mut doc, 1, &mut visited, None, None).unwrap();

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
        author_field.relationship = Some(RelationshipConfig {
            collection: "authors".to_string(),
            has_many: false,
            max_depth: None,
            polymorphic: vec![],
        });
        let mut editor_field = make_field("editor", FieldType::Relationship);
        editor_field.relationship = Some(RelationshipConfig {
            collection: "authors".to_string(),
            has_many: false,
            max_depth: None,
            polymorphic: vec![],
        });
        let posts_def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            author_field,
            editor_field,
        ]);

        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(make_authors_def());

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Hello"));
        doc.fields.insert("author".to_string(), serde_json::json!("a1"));
        doc.fields.insert("editor".to_string(), serde_json::json!("a1"));

        let mut visited = HashSet::new();
        let select = vec!["author".to_string()]; // Only populate author, not editor
        populate_relationships(
            &conn, &registry, "posts", &posts_def,
            &mut doc, 1, &mut visited, Some(&select), None).unwrap();

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
        doc.fields.insert("title".to_string(), serde_json::json!("Hello"));
        doc.fields.insert("author".to_string(), serde_json::json!(""));

        let mut visited = HashSet::new();
        populate_relationships(
            &conn, &registry, "posts", &posts_def,
            &mut doc, 1, &mut visited, None, None).unwrap();

        // Empty string ID should be skipped (the `_ => continue` branch)
        assert_eq!(
            doc.fields.get("author").and_then(|v| v.as_str()),
            Some(""),
            "empty string author should not be populated"
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
                VALUES ('p1', 'Hello', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let cats_def = make_collection_def("categories", vec![
            make_field("name", FieldType::Text),
        ]);
        let mut tags_field = make_field("tags", FieldType::Relationship);
        tags_field.relationship = Some(RelationshipConfig {
            collection: "categories".to_string(),
            has_many: true,
            max_depth: None,
            polymorphic: vec![],
        });
        let posts_def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            tags_field,
        ]);

        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(cats_def);

        let mut doc = Document::new("p1".to_string());
        doc.fields.insert("tags".to_string(), serde_json::json!(["c1"]));

        // Pre-mark c1 as visited — should keep it as ID string
        let mut visited = HashSet::new();
        visited.insert(("categories".to_string(), "c1".to_string()));

        populate_relationships(
            &conn, &registry, "posts", &posts_def,
            &mut doc, 1, &mut visited, None, None).unwrap();

        let tags = doc.fields.get("tags").expect("tags should exist");
        let arr = tags.as_array().expect("tags should be array");
        assert_eq!(arr.len(), 1);
        // Already visited — should remain as string ID
        assert_eq!(arr[0].as_str(), Some("c1"));
    }

    // ── Join field tests ──────────────────────────────────────────────────

    fn setup_join_db() -> Connection {
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
            INSERT INTO authors (id, name, created_at, updated_at)
                VALUES ('a1', 'Alice', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, author, created_at, updated_at)
                VALUES ('p1', 'First Post', 'a1', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, author, created_at, updated_at)
                VALUES ('p2', 'Second Post', 'a1', '2024-01-01', '2024-01-01');
            INSERT INTO posts (id, title, author, created_at, updated_at)
                VALUES ('p3', 'Other Post', 'a2', '2024-01-01', '2024-01-01');"
        ).unwrap();
        conn
    }

    fn make_authors_def_with_join() -> CollectionDefinition {
        let mut join_field = make_field("posts", FieldType::Join);
        join_field.join = Some(JoinConfig {
            collection: "posts".to_string(),
            on: "author".to_string(),
        });
        make_collection_def("authors", vec![
            make_field("name", FieldType::Text),
            join_field,
        ])
    }

    fn make_posts_def_for_join() -> CollectionDefinition {
        let mut author_field = make_field("author", FieldType::Relationship);
        author_field.relationship = Some(RelationshipConfig {
            collection: "authors".to_string(),
            has_many: false,
            max_depth: None,
            polymorphic: vec![],
        });
        make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            author_field,
        ])
    }

    #[test]
    fn join_field_populates_reverse_docs() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut doc = Document::new("a1".to_string());
        doc.fields.insert("name".to_string(), serde_json::json!("Alice"));

        let mut visited = HashSet::new();
        populate_relationships(
            &conn, &registry, "authors", &authors_def,
            &mut doc, 1, &mut visited, None, None).unwrap();

        let posts = doc.fields.get("posts").expect("posts join field should exist");
        let arr = posts.as_array().expect("posts should be an array");
        assert_eq!(arr.len(), 2, "Alice has 2 posts");

        let titles: Vec<&str> = arr.iter()
            .filter_map(|v| v.get("title").and_then(|t| t.as_str()))
            .collect();
        assert!(titles.contains(&"First Post"));
        assert!(titles.contains(&"Second Post"));
    }

    #[test]
    fn join_field_depth_zero_noop() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut doc = Document::new("a1".to_string());
        doc.fields.insert("name".to_string(), serde_json::json!("Alice"));

        let mut visited = HashSet::new();
        populate_relationships(
            &conn, &registry, "authors", &authors_def,
            &mut doc, 0, &mut visited, None, None).unwrap();

        // At depth=0, join field should not be populated
        assert!(doc.fields.get("posts").is_none(), "depth=0 should not add join field");
    }

    #[test]
    fn join_field_no_matching_docs() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        // Author with no posts
        let mut doc = Document::new("a99".to_string());
        doc.fields.insert("name".to_string(), serde_json::json!("Nobody"));

        let mut visited = HashSet::new();
        populate_relationships(
            &conn, &registry, "authors", &authors_def,
            &mut doc, 1, &mut visited, None, None).unwrap();

        let posts = doc.fields.get("posts").expect("posts join field should exist");
        let arr = posts.as_array().expect("posts should be an array");
        assert!(arr.is_empty(), "no matching posts should produce empty array");
    }

    #[test]
    fn join_field_select_filters() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut doc = Document::new("a1".to_string());
        doc.fields.insert("name".to_string(), serde_json::json!("Alice"));

        let mut visited = HashSet::new();
        // Select only "name", not "posts"
        let select = vec!["name".to_string()];
        populate_relationships(
            &conn, &registry, "authors", &authors_def,
            &mut doc, 1, &mut visited, Some(&select), None).unwrap();

        // Join field should be skipped because it's not in select
        assert!(doc.fields.get("posts").is_none(), "join field not in select should be skipped");
    }

    // ── Polymorphic relationship population ─────────────────────────────────

    fn setup_polymorphic_populate_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE entries (
                id TEXT PRIMARY KEY,
                title TEXT,
                related TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE articles (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE pages (
                id TEXT PRIMARY KEY,
                title TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            -- Polymorphic has-many junction table
            CREATE TABLE entries_refs (
                parent_id TEXT,
                related_id TEXT,
                related_collection TEXT NOT NULL DEFAULT '',
                _order INTEGER,
                PRIMARY KEY (parent_id, related_id, related_collection)
            );
            INSERT INTO articles VALUES ('a1', 'Article One', '2024-01-01', '2024-01-01');
            INSERT INTO pages VALUES ('pg1', 'Page One', '2024-01-01', '2024-01-01');
            INSERT INTO entries VALUES ('e1', 'Entry', 'articles/a1', '2024-01-01', '2024-01-01');"
        ).unwrap();
        conn
    }

    fn make_entries_def_poly_has_one() -> CollectionDefinition {
        let mut related_field = make_field("related", FieldType::Relationship);
        related_field.relationship = Some(RelationshipConfig {
            collection: "articles".to_string(),
            has_many: false,
            max_depth: None,
            polymorphic: vec!["articles".to_string(), "pages".to_string()],
        });
        make_collection_def("entries", vec![
            make_field("title", FieldType::Text),
            related_field,
        ])
    }

    fn make_entries_def_poly_has_many() -> CollectionDefinition {
        let mut refs_field = make_field("refs", FieldType::Relationship);
        refs_field.relationship = Some(RelationshipConfig {
            collection: "articles".to_string(),
            has_many: true,
            max_depth: None,
            polymorphic: vec!["articles".to_string(), "pages".to_string()],
        });
        make_collection_def("entries", vec![
            make_field("title", FieldType::Text),
            refs_field,
        ])
    }

    #[test]
    fn populate_polymorphic_has_one() {
        let conn = setup_polymorphic_populate_db();
        let entries_def = make_entries_def_poly_has_one();
        let articles_def = make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
        let pages_def = make_collection_def("pages", vec![make_field("title", FieldType::Text)]);
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());
        registry.register_collection(articles_def);
        registry.register_collection(pages_def);

        let mut doc = Document::new("e1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Entry"));
        doc.fields.insert("related".to_string(), serde_json::json!("articles/a1"));

        let mut visited = HashSet::new();
        populate_relationships(
            &conn, &registry, "entries", &entries_def,
            &mut doc, 1, &mut visited, None, None).unwrap();

        // Should be populated as a full document object
        let related = doc.fields.get("related").expect("related should exist");
        assert!(related.is_object(), "polymorphic has-one should be populated to object");
        assert_eq!(related.get("id").and_then(|v| v.as_str()), Some("a1"));
        assert_eq!(related.get("title").and_then(|v| v.as_str()), Some("Article One"));
        assert_eq!(related.get("collection").and_then(|v| v.as_str()), Some("articles"));
    }

    #[test]
    fn populate_polymorphic_has_one_depth_zero() {
        let conn = setup_polymorphic_populate_db();
        let entries_def = make_entries_def_poly_has_one();
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());

        let mut doc = Document::new("e1".to_string());
        doc.fields.insert("related".to_string(), serde_json::json!("articles/a1"));

        let mut visited = HashSet::new();
        populate_relationships(
            &conn, &registry, "entries", &entries_def,
            &mut doc, 0, &mut visited, None, None).unwrap();

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
                VALUES ('e1', 'a1', 'articles', 0), ('e1', 'pg1', 'pages', 1);"
        ).unwrap();

        let entries_def = make_entries_def_poly_has_many();
        let articles_def = make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
        let pages_def = make_collection_def("pages", vec![make_field("title", FieldType::Text)]);
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());
        registry.register_collection(articles_def);
        registry.register_collection(pages_def);

        // Hydrate first (loads polymorphic has-many from junction table)
        let mut doc = Document::new("e1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Entry"));
        crate::db::query::join::hydrate_document(
            &conn, "entries", &entries_def.fields, &mut doc, None, None,
        ).unwrap();

        // Verify hydration produced composite strings
        let refs = doc.fields.get("refs").expect("refs should be hydrated");
        let arr = refs.as_array().unwrap();
        assert_eq!(arr[0].as_str().unwrap(), "articles/a1");
        assert_eq!(arr[1].as_str().unwrap(), "pages/pg1");

        // Now populate
        let mut visited = HashSet::new();
        populate_relationships(
            &conn, &registry, "entries", &entries_def,
            &mut doc, 1, &mut visited, None, None).unwrap();

        let refs = doc.fields.get("refs").expect("refs should exist after populate");
        let arr = refs.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        // First item: article
        assert!(arr[0].is_object(), "item should be populated object");
        assert_eq!(arr[0].get("id").and_then(|v| v.as_str()), Some("a1"));
        assert_eq!(arr[0].get("collection").and_then(|v| v.as_str()), Some("articles"));
        // Second item: page
        assert!(arr[1].is_object());
        assert_eq!(arr[1].get("id").and_then(|v| v.as_str()), Some("pg1"));
        assert_eq!(arr[1].get("collection").and_then(|v| v.as_str()), Some("pages"));
    }

    #[test]
    fn populate_polymorphic_unknown_collection_keeps_string() {
        let conn = setup_polymorphic_populate_db();
        let entries_def = make_entries_def_poly_has_one();
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());
        // Don't register "articles" — it's unknown

        let mut doc = Document::new("e1".to_string());
        doc.fields.insert("related".to_string(), serde_json::json!("unknown_col/x1"));

        let mut visited = HashSet::new();
        populate_relationships(
            &conn, &registry, "entries", &entries_def,
            &mut doc, 1, &mut visited, None, None).unwrap();

        // Unknown collection: value stays as string
        assert_eq!(
            doc.fields.get("related").and_then(|v| v.as_str()),
            Some("unknown_col/x1")
        );
    }

    // ── populate_relationships_batch tests ──────────────────────────────────

    #[test]
    fn batch_depth_zero_noop() {
        let conn = setup_populate_db();
        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        let mut docs = vec![];
        populate_relationships_batch(
            &conn, &registry, "posts", &posts_def, &mut docs, 0, None, None,
        ).unwrap();
        // Empty docs + depth 0 → no-op, no error
    }

    #[test]
    fn batch_empty_docs_noop() {
        let conn = setup_populate_db();
        let registry = make_registry_with_posts_and_authors();
        let posts_def = make_posts_def();

        let mut docs = vec![];
        populate_relationships_batch(
            &conn, &registry, "posts", &posts_def, &mut docs, 1, None, None,
        ).unwrap();
    }

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
                d.fields.insert("title".to_string(), serde_json::json!("Post 1"));
                d.fields.insert("author".to_string(), serde_json::json!("a1"));
                d
            },
            {
                let mut d = Document::new("p2".to_string());
                d.fields.insert("title".to_string(), serde_json::json!("Post 2"));
                d.fields.insert("author".to_string(), serde_json::json!("a1"));
                d
            },
            {
                let mut d = Document::new("p3".to_string());
                d.fields.insert("title".to_string(), serde_json::json!("Post 3"));
                d.fields.insert("author".to_string(), serde_json::json!("a2"));
                d
            },
        ];

        populate_relationships_batch(
            &conn, &registry, "posts", &posts_def, &mut docs, 1, None, None,
        ).unwrap();

        // All three should have populated authors
        for (i, doc) in docs.iter().enumerate() {
            let author = doc.fields.get("author").unwrap_or_else(|| panic!("doc {} missing author", i));
            assert!(author.is_object(), "doc {} author should be object, got {:?}", i, author);
        }
        // p1 and p2 share the same author
        assert_eq!(docs[0].fields["author"].get("id").unwrap().as_str(), Some("a1"));
        assert_eq!(docs[0].fields["author"].get("name").unwrap().as_str(), Some("Alice"));
        assert_eq!(docs[1].fields["author"].get("id").unwrap().as_str(), Some("a1"));
        assert_eq!(docs[2].fields["author"].get("id").unwrap().as_str(), Some("a2"));
        assert_eq!(docs[2].fields["author"].get("name").unwrap().as_str(), Some("Bob"));
    }

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
        tags_field.relationship = Some(RelationshipConfig {
            collection: "categories".to_string(),
            has_many: true,
            max_depth: None,
            polymorphic: vec![],
        });
        let posts_def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            tags_field,
        ]);

        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(cats_def);

        let mut docs = vec![
            {
                let mut d = Document::new("p1".to_string());
                d.fields.insert("tags".to_string(), serde_json::json!(["c1", "c2"]));
                d
            },
            {
                let mut d = Document::new("p2".to_string());
                d.fields.insert("tags".to_string(), serde_json::json!(["c2", "c3"]));
                d
            },
        ];

        populate_relationships_batch(
            &conn, &registry, "posts", &posts_def, &mut docs, 1, None, None,
        ).unwrap();

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
        author_field.relationship = Some(RelationshipConfig {
            collection: "authors".to_string(),
            has_many: false,
            max_depth: None,
            polymorphic: vec![],
        });
        let mut editor_field = make_field("editor", FieldType::Relationship);
        editor_field.relationship = Some(RelationshipConfig {
            collection: "authors".to_string(),
            has_many: false,
            max_depth: None,
            polymorphic: vec![],
        });
        let posts_def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            author_field,
            editor_field,
        ]);
        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(make_authors_def());

        let mut docs = vec![{
            let mut d = Document::new("p1".to_string());
            d.fields.insert("author".to_string(), serde_json::json!("a1"));
            d.fields.insert("editor".to_string(), serde_json::json!("a1"));
            d
        }];

        let select = vec!["author".to_string()];
        populate_relationships_batch(
            &conn, &registry, "posts", &posts_def, &mut docs, 1, Some(&select), None,
        ).unwrap();

        // author should be populated
        assert!(docs[0].fields["author"].is_object());
        // editor should remain as ID (not in select)
        assert_eq!(docs[0].fields["editor"].as_str(), Some("a1"));
    }

    #[test]
    fn batch_max_depth_zero_stays_as_id() {
        let conn = setup_populate_db();

        let mut author_field = make_field("author", FieldType::Relationship);
        author_field.relationship = Some(RelationshipConfig {
            collection: "authors".to_string(),
            has_many: false,
            max_depth: Some(0),
            polymorphic: vec![],
        });
        let posts_def = make_collection_def("posts", vec![
            make_field("title", FieldType::Text),
            author_field,
        ]);
        let mut registry = Registry::new();
        registry.register_collection(posts_def.clone());
        registry.register_collection(make_authors_def());

        let mut docs = vec![{
            let mut d = Document::new("p1".to_string());
            d.fields.insert("author".to_string(), serde_json::json!("a1"));
            d
        }];

        populate_relationships_batch(
            &conn, &registry, "posts", &posts_def, &mut docs, 1, None, None,
        ).unwrap();

        // max_depth=0 should prevent population
        assert_eq!(docs[0].fields["author"].as_str(), Some("a1"));
    }

    #[test]
    fn batch_missing_related_docs_stay_as_ids() {
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
            d.fields.insert("author".to_string(), serde_json::json!("nonexistent"));
            d
        }];

        populate_relationships_batch(
            &conn, &registry, "posts", &posts_def, &mut docs, 1, None, None,
        ).unwrap();

        // Missing doc stays as ID string
        assert_eq!(docs[0].fields["author"].as_str(), Some("nonexistent"));
    }

    #[test]
    fn batch_with_join_field() {
        let conn = setup_join_db();
        let authors_def = make_authors_def_with_join();
        let posts_def = make_posts_def_for_join();
        let mut registry = Registry::new();
        registry.register_collection(authors_def.clone());
        registry.register_collection(posts_def);

        let mut docs = vec![
            {
                let mut d = Document::new("a1".to_string());
                d.fields.insert("name".to_string(), serde_json::json!("Alice"));
                d
            },
        ];

        populate_relationships_batch(
            &conn, &registry, "authors", &authors_def, &mut docs, 1, None, None,
        ).unwrap();

        let posts = docs[0].fields.get("posts").expect("join field should be populated");
        let arr = posts.as_array().unwrap();
        assert_eq!(arr.len(), 2, "Alice has 2 posts");
    }

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
            d.fields.insert("title".to_string(), serde_json::json!("Entry"));
            d.fields.insert("related".to_string(), serde_json::json!("articles/a1"));
            d
        }];

        populate_relationships_batch(
            &conn, &registry, "entries", &entries_def, &mut docs, 1, None, None,
        ).unwrap();

        let related = &docs[0].fields["related"];
        assert!(related.is_object(), "polymorphic has-one should be populated");
        assert_eq!(related.get("id").unwrap().as_str(), Some("a1"));
        assert_eq!(related.get("collection").unwrap().as_str(), Some("articles"));
    }

    #[test]
    fn batch_polymorphic_has_many() {
        let conn = setup_polymorphic_populate_db();
        conn.execute_batch(
            "INSERT INTO entries_refs (parent_id, related_id, related_collection, _order)
                VALUES ('e1', 'a1', 'articles', 0), ('e1', 'pg1', 'pages', 1);"
        ).unwrap();

        let entries_def = make_entries_def_poly_has_many();
        let articles_def = make_collection_def("articles", vec![make_field("title", FieldType::Text)]);
        let pages_def = make_collection_def("pages", vec![make_field("title", FieldType::Text)]);
        let mut registry = Registry::new();
        registry.register_collection(entries_def.clone());
        registry.register_collection(articles_def);
        registry.register_collection(pages_def);

        let mut doc = Document::new("e1".to_string());
        doc.fields.insert("title".to_string(), serde_json::json!("Entry"));
        crate::db::query::join::hydrate_document(
            &conn, "entries", &entries_def.fields, &mut doc, None, None,
        ).unwrap();

        let mut docs = vec![doc];
        populate_relationships_batch(
            &conn, &registry, "entries", &entries_def, &mut docs, 1, None, None,
        ).unwrap();

        let refs = docs[0].fields.get("refs").unwrap();
        let arr = refs.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr[0].is_object());
        assert_eq!(arr[0].get("collection").unwrap().as_str(), Some("articles"));
        assert!(arr[1].is_object());
        assert_eq!(arr[1].get("collection").unwrap().as_str(), Some("pages"));
    }
}
