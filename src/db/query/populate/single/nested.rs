//! Nested container population — walks Groups, Blocks, and Arrays to populate
//! relationship/upload fields inside `serde_json::Map` values.

use anyhow::Result;
use serde_json::{Map, Value};
use std::collections::HashSet;

use super::populate_relationships_cached;
use crate::core::{
    BlockDefinition, CollectionDefinition, Document, FieldDefinition, FieldType,
    field::flatten_array_sub_fields, upload,
};
use crate::db::query::populate::{
    PopulateContext, PopulateCtx, PopulateOpts, document_to_json, locale_cache_key, parse_poly_ref,
    populate_cache_key,
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
            FieldType::Group => populate_group_in_doc(pctx, doc, field, visited)?,
            FieldType::Blocks => populate_blocks_in_doc(pctx, doc, field, visited)?,
            FieldType::Array => populate_array_in_doc(pctx, doc, field, visited)?,
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

/// Populate relationship fields inside a Group value.
fn populate_group_in_doc(
    pctx: &PopulateCtx<'_>,
    doc: &mut Document,
    field: &FieldDefinition,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let Some(Value::Object(mut map)) = doc.fields.remove(&field.name) else {
        return Ok(());
    };

    let flat = flatten_array_sub_fields(&field.fields);
    populate_in_map(pctx, &mut map, &flat, visited)?;
    doc.fields.insert(field.name.clone(), Value::Object(map));

    Ok(())
}

/// Populate relationship fields inside a Blocks value.
fn populate_blocks_in_doc(
    pctx: &PopulateCtx<'_>,
    doc: &mut Document,
    field: &FieldDefinition,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let Some(Value::Array(mut items)) = doc.fields.remove(&field.name) else {
        return Ok(());
    };

    populate_block_items(pctx, &mut items, &field.blocks, visited)?;
    doc.fields.insert(field.name.clone(), Value::Array(items));

    Ok(())
}

/// Populate relationship fields inside an Array value.
fn populate_array_in_doc(
    pctx: &PopulateCtx<'_>,
    doc: &mut Document,
    field: &FieldDefinition,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let Some(Value::Array(mut items)) = doc.fields.remove(&field.name) else {
        return Ok(());
    };

    let flat = flatten_array_sub_fields(&field.fields);

    for item in &mut items {
        if let Value::Object(map) = item {
            populate_in_map(pctx, map, &flat, visited)?;
        }
    }

    doc.fields.insert(field.name.clone(), Value::Array(items));

    Ok(())
}

/// Populate block items by matching `_block_type` to block definitions.
fn populate_block_items(
    pctx: &PopulateCtx<'_>,
    items: &mut [Value],
    blocks: &[BlockDefinition],
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
                populate_group_in_map(pctx, map, field, visited)?;
            }
            FieldType::Blocks => {
                populate_blocks_in_map(pctx, map, field, visited)?;
            }
            FieldType::Array => {
                populate_array_items_in_map(pctx, map, field, visited)?;
            }
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

/// Populate a Group field within a JSON map.
fn populate_group_in_map(
    pctx: &PopulateCtx<'_>,
    map: &mut Map<String, Value>,
    field: &FieldDefinition,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let Some(Value::Object(mut inner)) = map.remove(&field.name) else {
        return Ok(());
    };

    let flat = flatten_array_sub_fields(&field.fields);
    populate_in_map(pctx, &mut inner, &flat, visited)?;
    map.insert(field.name.clone(), Value::Object(inner));

    Ok(())
}

/// Populate a Blocks field within a JSON map.
fn populate_blocks_in_map(
    pctx: &PopulateCtx<'_>,
    map: &mut Map<String, Value>,
    field: &FieldDefinition,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let Some(Value::Array(mut items)) = map.remove(&field.name) else {
        return Ok(());
    };

    populate_block_items(pctx, &mut items, &field.blocks, visited)?;
    map.insert(field.name.clone(), Value::Array(items));

    Ok(())
}

/// Populate an Array field within a JSON map.
fn populate_array_items_in_map(
    pctx: &PopulateCtx<'_>,
    map: &mut Map<String, Value>,
    field: &FieldDefinition,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    let Some(Value::Array(mut items)) = map.remove(&field.name) else {
        return Ok(());
    };

    let flat = flatten_array_sub_fields(&field.fields);

    for item in &mut items {
        if let Value::Object(m) = item {
            populate_in_map(pctx, m, &flat, visited)?;
        }
    }

    map.insert(field.name.clone(), Value::Array(items));

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

use crate::db::query::populate::helpers::{cache_get_doc, cache_set_doc};

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

    let locale_key = locale_cache_key(pctx.locale_ctx);
    let key = populate_cache_key(rel_collection, &id, locale_key.as_deref());

    if let Some(cached) = cache_get_doc(pctx.cache, &key)? {
        map.insert(name.to_string(), document_to_json(&cached, rel_collection));
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

        let _ = cache_set_doc(pctx.cache, &key, &related_doc);
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

        let locale_key = locale_cache_key(pctx.locale_ctx);
        let key = populate_cache_key(rel_collection, id, locale_key.as_deref());

        if let Some(cached) = cache_get_doc(pctx.cache, &key)? {
            populated.push(document_to_json(&cached, rel_collection));
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

            let _ = cache_set_doc(pctx.cache, &key, &related_doc);
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

    let locale_key = locale_cache_key(pctx.locale_ctx);
    let key = populate_cache_key(&col, &id, locale_key.as_deref());

    if let Some(cached) = cache_get_doc(pctx.cache, &key)? {
        map.insert(name.to_string(), document_to_json(&cached, &col));
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

        let _ = cache_set_doc(pctx.cache, &key, &rd);
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

        let locale_key = locale_cache_key(pctx.locale_ctx);
        let key = populate_cache_key(&col, &id, locale_key.as_deref());

        if let Some(cached) = cache_get_doc(pctx.cache, &key)? {
            populated.push(document_to_json(&cached, &col));
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

            let _ = cache_set_doc(pctx.cache, &key, &rd);
            populated.push(document_to_json(&rd, &col));
        } else {
            populated.push(Value::String(item.clone()));
        }
    }
    map.insert(name.to_string(), Value::Array(populated));
    Ok(())
}
