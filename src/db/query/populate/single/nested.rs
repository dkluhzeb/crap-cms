//! Nested container population — walks Groups, Blocks, and Arrays to populate
//! relationship/upload fields inside `serde_json::Map` values.

use anyhow::Result;
use serde_json::{Map, Value};
use std::collections::HashSet;

use super::populate_relationships_cached;
use crate::core::{
    CollectionDefinition, Document, FieldDefinition, FieldType, field::flatten_array_sub_fields,
    upload,
};
use crate::db::query::populate::{
    MAX_POPULATE_CACHE_SIZE, PopulateContext, PopulateCtx, PopulateOpts, document_to_json,
    parse_poly_ref,
};
use crate::db::query::read::find_by_id;

/// Walk top-level container fields (Group, Blocks, Array) in a document and
/// populate any relationship/upload sub-fields within them.
pub(crate) fn populate_containers_in_doc(
    pctx: &PopulateCtx<'_>,
    doc: &mut Document,
    fields: &[FieldDefinition],
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                if let Some(Value::Object(map)) = doc.fields.remove(&field.name) {
                    let mut map = map;
                    let flat = flatten_array_sub_fields(&field.fields);
                    populate_in_map(pctx, &mut map, &flat, visited)?;
                    doc.fields.insert(field.name.clone(), Value::Object(map));
                }
            }
            FieldType::Blocks => {
                if let Some(Value::Array(items)) = doc.fields.remove(&field.name) {
                    let mut items = items;
                    populate_block_items(pctx, &mut items, &field.blocks, visited)?;
                    doc.fields.insert(field.name.clone(), Value::Array(items));
                }
            }
            FieldType::Array => {
                if let Some(Value::Array(items)) = doc.fields.remove(&field.name) {
                    let mut items = items;
                    let flat = flatten_array_sub_fields(&field.fields);
                    for item in &mut items {
                        if let Value::Object(map) = item {
                            populate_in_map(pctx, map, &flat, visited)?;
                        }
                    }
                    doc.fields.insert(field.name.clone(), Value::Array(items));
                }
            }
            // Transparent containers: recurse to find nested Groups/Blocks/Arrays
            FieldType::Row | FieldType::Collapsible => {
                populate_containers_in_doc(pctx, doc, &field.fields, visited)?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    populate_containers_in_doc(pctx, doc, &tab.fields, visited)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Populate block items by matching `_block_type` to block definitions.
fn populate_block_items(
    pctx: &PopulateCtx<'_>,
    items: &mut [Value],
    blocks: &[crate::core::field::BlockDefinition],
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    for item in items.iter_mut() {
        if let Value::Object(map) = item {
            let block_type = map
                .get("_block_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if let Some(block_def) = blocks.iter().find(|b| b.block_type == block_type) {
                let flat = flatten_array_sub_fields(&block_def.fields);
                populate_in_map(pctx, map, &flat, visited)?;
            }
        }
    }
    Ok(())
}

/// Recursively populate relationship/upload fields within a JSON map.
fn populate_in_map(
    pctx: &PopulateCtx<'_>,
    map: &mut Map<String, Value>,
    fields: &[&FieldDefinition],
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    for field in fields {
        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                populate_rel_in_map(pctx, map, field, visited)?;
            }
            FieldType::Group => {
                if let Some(Value::Object(inner)) = map.remove(&field.name) {
                    let mut inner = inner;
                    let flat = flatten_array_sub_fields(&field.fields);
                    populate_in_map(pctx, &mut inner, &flat, visited)?;
                    map.insert(field.name.clone(), Value::Object(inner));
                }
            }
            FieldType::Blocks => {
                if let Some(Value::Array(items)) = map.remove(&field.name) {
                    let mut items = items;
                    populate_block_items(pctx, &mut items, &field.blocks, visited)?;
                    map.insert(field.name.clone(), Value::Array(items));
                }
            }
            FieldType::Array => {
                if let Some(Value::Array(items)) = map.remove(&field.name) {
                    let mut items = items;
                    let flat = flatten_array_sub_fields(&field.fields);
                    for item in &mut items {
                        if let Value::Object(m) = item {
                            populate_in_map(pctx, m, &flat, visited)?;
                        }
                    }
                    map.insert(field.name.clone(), Value::Array(items));
                }
            }
            // Transparent containers within nested data (e.g., Row inside a Block)
            FieldType::Row | FieldType::Collapsible => {
                let flat = flatten_array_sub_fields(&field.fields);
                populate_in_map(pctx, map, &flat, visited)?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    let flat = flatten_array_sub_fields(&tab.fields);
                    populate_in_map(pctx, map, &flat, visited)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Dispatch relationship population for a field within a JSON map.
fn populate_rel_in_map(
    pctx: &PopulateCtx<'_>,
    map: &mut Map<String, Value>,
    field: &FieldDefinition,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let rel = match &field.relationship {
        Some(rc) => rc,
        None => return Ok(()),
    };

    let effective_depth = match rel.max_depth {
        Some(max) if max < pctx.effective_depth => max,
        _ => pctx.effective_depth,
    };

    if effective_depth <= 0 {
        return Ok(());
    }

    if rel.is_polymorphic() {
        if rel.has_many {
            populate_poly_has_many_in_map(pctx, map, &field.name, effective_depth, visited)?;
        } else {
            populate_poly_has_one_in_map(pctx, map, &field.name, effective_depth, visited)?;
        }
    } else {
        let rel_def = match pctx.registry.get_collection(&rel.collection) {
            Some(d) => d.clone(),
            None => return Ok(()),
        };

        if rel.has_many {
            populate_has_many_in_map(
                pctx,
                map,
                &field.name,
                &rel.collection,
                &rel_def,
                effective_depth,
                visited,
            )?;
        } else {
            populate_has_one_in_map(
                pctx,
                map,
                &field.name,
                &rel.collection,
                &rel_def,
                effective_depth,
                visited,
            )?;
        }
    }
    Ok(())
}

/// Populate a non-polymorphic has-one field within a JSON map.
fn populate_has_one_in_map(
    pctx: &PopulateCtx<'_>,
    map: &mut Map<String, Value>,
    name: &str,
    rel_collection: &str,
    rel_def: &CollectionDefinition,
    effective_depth: i32,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let id = match map.get(name) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Ok(()),
    };

    if visited.contains(&(rel_collection.to_string(), id.clone())) {
        return Ok(());
    }

    let cache_key = (rel_collection.to_string(), id.clone());

    if let Some(cached) = pctx.cache.get(&cache_key) {
        map.insert(
            name.to_string(),
            document_to_json(cached.value(), rel_collection),
        );
    } else if let Some(mut related_doc) =
        find_by_id(pctx.conn, rel_collection, rel_def, &id, pctx.locale_ctx)?
    {
        if let Some(ref uc) = rel_def.upload
            && uc.enabled
        {
            upload::assemble_sizes_object(&mut related_doc, uc);
        }
        populate_relationships_cached(
            &PopulateContext {
                conn: pctx.conn,
                registry: pctx.registry,
                collection_slug: rel_collection,
                def: rel_def,
            },
            &mut related_doc,
            visited,
            &PopulateOpts {
                depth: effective_depth - 1,
                select: None,
                locale_ctx: pctx.locale_ctx,
            },
            pctx.cache,
        )?;
        if pctx.cache.len() < MAX_POPULATE_CACHE_SIZE {
            pctx.cache.insert(cache_key, related_doc.clone());
        }
        map.insert(
            name.to_string(),
            document_to_json(&related_doc, rel_collection),
        );
    }
    Ok(())
}

/// Populate a non-polymorphic has-many field within a JSON map.
fn populate_has_many_in_map(
    pctx: &PopulateCtx<'_>,
    map: &mut Map<String, Value>,
    name: &str,
    rel_collection: &str,
    rel_def: &CollectionDefinition,
    effective_depth: i32,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let ids: Vec<String> = match map.get(name) {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => return Ok(()),
    };

    let mut populated = Vec::new();
    for id in &ids {
        if visited.contains(&(rel_collection.to_string(), id.clone())) {
            populated.push(Value::String(id.clone()));
            continue;
        }
        let cache_key = (rel_collection.to_string(), id.clone());

        if let Some(cached) = pctx.cache.get(&cache_key) {
            populated.push(document_to_json(cached.value(), rel_collection));
        } else if let Some(mut related_doc) =
            find_by_id(pctx.conn, rel_collection, rel_def, id, pctx.locale_ctx)?
        {
            if let Some(ref uc) = rel_def.upload
                && uc.enabled
            {
                upload::assemble_sizes_object(&mut related_doc, uc);
            }
            populate_relationships_cached(
                &PopulateContext {
                    conn: pctx.conn,
                    registry: pctx.registry,
                    collection_slug: rel_collection,
                    def: rel_def,
                },
                &mut related_doc,
                visited,
                &PopulateOpts {
                    depth: effective_depth - 1,
                    select: None,
                    locale_ctx: pctx.locale_ctx,
                },
                pctx.cache,
            )?;
            if pctx.cache.len() < MAX_POPULATE_CACHE_SIZE {
                pctx.cache.insert(cache_key, related_doc.clone());
            }
            populated.push(document_to_json(&related_doc, rel_collection));
        } else {
            populated.push(Value::String(id.clone()));
        }
    }
    map.insert(name.to_string(), Value::Array(populated));
    Ok(())
}

/// Populate a polymorphic has-one field within a JSON map.
fn populate_poly_has_one_in_map(
    pctx: &PopulateCtx<'_>,
    map: &mut Map<String, Value>,
    name: &str,
    effective_depth: i32,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let raw = match map.get(name) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Ok(()),
    };

    let (col, id) = match parse_poly_ref(&raw) {
        Some(pair) => pair,
        None => return Ok(()),
    };

    if visited.contains(&(col.clone(), id.clone())) {
        return Ok(());
    }

    let item_def = match pctx.registry.get_collection(&col) {
        Some(d) => d.clone(),
        None => return Ok(()),
    };

    let cache_key = (col.clone(), id.clone());

    if let Some(cached) = pctx.cache.get(&cache_key) {
        map.insert(name.to_string(), document_to_json(cached.value(), &col));
    } else if let Some(mut rd) = find_by_id(pctx.conn, &col, &item_def, &id, pctx.locale_ctx)? {
        if let Some(ref uc) = item_def.upload
            && uc.enabled
        {
            upload::assemble_sizes_object(&mut rd, uc);
        }
        populate_relationships_cached(
            &PopulateContext {
                conn: pctx.conn,
                registry: pctx.registry,
                collection_slug: &col,
                def: &item_def,
            },
            &mut rd,
            visited,
            &PopulateOpts {
                depth: effective_depth - 1,
                select: None,
                locale_ctx: pctx.locale_ctx,
            },
            pctx.cache,
        )?;
        if pctx.cache.len() < MAX_POPULATE_CACHE_SIZE {
            pctx.cache.insert(cache_key, rd.clone());
        }
        map.insert(name.to_string(), document_to_json(&rd, &col));
    }
    Ok(())
}

/// Populate a polymorphic has-many field within a JSON map.
fn populate_poly_has_many_in_map(
    pctx: &PopulateCtx<'_>,
    map: &mut Map<String, Value>,
    name: &str,
    effective_depth: i32,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let items: Vec<String> = match map.get(name) {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => return Ok(()),
    };

    let mut populated = Vec::new();
    for item in &items {
        let (col, id) = match parse_poly_ref(item) {
            Some(pair) => pair,
            None => {
                populated.push(Value::String(item.clone()));
                continue;
            }
        };

        if visited.contains(&(col.clone(), id.clone())) {
            populated.push(Value::String(item.clone()));
            continue;
        }

        let item_def = match pctx.registry.get_collection(&col) {
            Some(d) => d.clone(),
            None => {
                populated.push(Value::String(item.clone()));
                continue;
            }
        };

        let cache_key = (col.clone(), id.clone());

        if let Some(cached) = pctx.cache.get(&cache_key) {
            populated.push(document_to_json(cached.value(), &col));
        } else if let Some(mut rd) = find_by_id(pctx.conn, &col, &item_def, &id, pctx.locale_ctx)? {
            if let Some(ref uc) = item_def.upload
                && uc.enabled
            {
                upload::assemble_sizes_object(&mut rd, uc);
            }
            populate_relationships_cached(
                &PopulateContext {
                    conn: pctx.conn,
                    registry: pctx.registry,
                    collection_slug: &col,
                    def: &item_def,
                },
                &mut rd,
                visited,
                &PopulateOpts {
                    depth: effective_depth - 1,
                    select: None,
                    locale_ctx: pctx.locale_ctx,
                },
                pctx.cache,
            )?;
            if pctx.cache.len() < MAX_POPULATE_CACHE_SIZE {
                pctx.cache.insert(cache_key, rd.clone());
            }
            populated.push(document_to_json(&rd, &col));
        } else {
            populated.push(Value::String(item.clone()));
        }
    }
    map.insert(name.to_string(), Value::Array(populated));
    Ok(())
}
