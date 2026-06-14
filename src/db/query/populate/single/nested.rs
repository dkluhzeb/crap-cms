//! Nested container population — walks Groups, Blocks, and Arrays to populate
//! relationship/upload fields inside `serde_json::Map` values.

use anyhow::Result;
use serde_json::{Map, Value};
use std::collections::HashSet;

use crate::core::{
    BlockDefinition, CollectionDefinition, Document, FieldDefinition, FieldType,
    field::flatten_array_sub_fields,
};
use crate::db::query::populate::{PopulateCtx, document_to_json, parse_poly_ref};

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
    let Some(rel) = &field.relationship else {
        return Ok(());
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

use super::dispatch::resolve_single_target;
use crate::db::query::AccessResult;
use crate::db::query::populate::helpers::resolve_target_access;
use std::collections::HashMap;

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

    let access = resolve_target_access(pctx, rel_collection, rel_def)?;

    if let Some(target) = resolve_single_target(
        pctx,
        rel_collection,
        rel_def,
        &id,
        &access,
        effective_depth,
        visited,
    )? {
        map.insert(name.to_string(), document_to_json(&target, rel_collection));
    }
    // Missing or hidden (draft/access): leave the raw id reference in place.

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
            .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
            .collect(),
        _ => return Ok(()),
    };

    let access = resolve_target_access(pctx, rel_collection, rel_def)?;

    let mut populated = Vec::new();
    for id in &ids {
        if visited.contains(&(rel_collection.to_string(), id.clone())) {
            populated.push(Value::String(id.clone()));
            continue;
        }

        match resolve_single_target(
            pctx,
            rel_collection,
            rel_def,
            id,
            &access,
            effective_depth,
            visited,
        )? {
            Some(target) => populated.push(document_to_json(&target, rel_collection)),
            // Missing or hidden: keep the raw id reference (nested-array behavior).
            None => populated.push(Value::String(id.clone())),
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
    let raw_ref = match map.get(name) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        _ => return Ok(()),
    };

    let Some((col, id)) = parse_poly_ref(&raw_ref) else {
        return Ok(());
    };

    if visited.contains(&(col.clone(), id.clone())) {
        return Ok(());
    }

    let item_def = match pctx.registry.get_collection(&col) {
        Some(d) => d.clone(),
        None => return Ok(()),
    };

    let access = resolve_target_access(pctx, &col, &item_def)?;

    if let Some(target) = resolve_single_target(
        pctx,
        &col,
        &item_def,
        &id,
        &access,
        effective_depth,
        visited,
    )? {
        map.insert(name.to_string(), document_to_json(&target, &col));
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
            .filter_map(|v| v.as_str().map(std::string::ToString::to_string))
            .collect(),
        _ => return Ok(()),
    };

    // Resolve each target collection's `read` access once for this field.
    let mut access_by: HashMap<String, AccessResult> = HashMap::new();

    let mut populated = Vec::new();
    for item in &items {
        let Some((col, id)) = parse_poly_ref(item) else {
            populated.push(Value::String(item.clone()));
            continue;
        };

        if visited.contains(&(col.clone(), id.clone())) {
            populated.push(Value::String(item.clone()));
            continue;
        }

        let Some(item_def) = pctx.registry.get_collection(&col).cloned() else {
            populated.push(Value::String(item.clone()));
            continue;
        };

        if !access_by.contains_key(&col) {
            let resolved = resolve_target_access(pctx, &col, &item_def)?;
            access_by.insert(col.clone(), resolved);
        }
        let access = access_by[&col].clone();

        match resolve_single_target(
            pctx,
            &col,
            &item_def,
            &id,
            &access,
            effective_depth,
            visited,
        )? {
            Some(target) => populated.push(document_to_json(&target, &col)),
            None => populated.push(Value::String(item.clone())),
        }
    }
    map.insert(name.to_string(), Value::Array(populated));
    Ok(())
}
