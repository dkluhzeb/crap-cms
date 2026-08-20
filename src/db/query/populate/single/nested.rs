//! Nested container population — walks Groups, Blocks, and Arrays to populate
//! relationship/upload fields inside `serde_json::Map` values.

use anyhow::Result;
use serde_json::{Map, Value};
use std::collections::HashSet;

use crate::core::{
    BlockDefinition, CollectionDefinition, Document, FieldDefinition, FieldType,
    field::flatten_array_sub_fields,
};
use crate::db::query::populate::{PopulateCtx, PopulateOpts, document_to_json, parse_poly_ref};

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
            FieldType::Join => {
                populate_join_in_map(pctx, map, field, visited)?;
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

/// Populate a `Join` (reverse-lookup) field within a JSON map. The join is
/// anchored to the *document's* id (`pctx.root_id`), not anything in the
/// container — a join nested inside a group/array/blocks/tab reverse-looks-up
/// the same rows it would at the top level. Gating (target read/draft access,
/// per-row visibility) is the shared join path's.
fn populate_join_in_map(
    pctx: &PopulateCtx<'_>,
    map: &mut Map<String, Value>,
    field: &FieldDefinition,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    if pctx.effective_depth <= 0 {
        return Ok(());
    }

    let Some(jc) = &field.join else {
        return Ok(());
    };

    let Some(target_def) = pctx.registry.get_collection(&jc.collection).cloned() else {
        return Ok(());
    };

    let opts = PopulateOpts {
        depth: pctx.effective_depth,
        select: None,
        locale_ctx: pctx.locale_ctx,
        published_only: pctx.published_only,
        join_access: pctx.join_access,
        user: pctx.user,
    };

    let populated = super::join::populate_join_docs(
        &super::join::JoinDocsCtx {
            conn: pctx.conn,
            registry: pctx.registry,
            cache: pctx.cache,
        },
        pctx.root_id,
        jc,
        &target_def,
        visited,
        &opts,
    )?;

    map.insert(field.name.clone(), Value::Array(populated));

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

    let effective_depth = rel.cap_depth(pctx.effective_depth);

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
use crate::db::query::populate::helpers::{TargetViews, resolve_target_views};
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

    let views = resolve_target_views(pctx, rel_collection, rel_def)?;

    let resolved = resolve_single_target(
        pctx,
        rel_collection,
        rel_def,
        &id,
        &views,
        effective_depth,
        visited,
    )?;

    // Resolve to the embedded target, or `null` when missing or access-hidden —
    // parity with the top-level has-one path, which never leaves a denied
    // target's raw id in place.
    let value = match resolved {
        Some(target) => document_to_json(&target, Some(rel_collection)),
        None => Value::Null,
    };
    map.insert(name.to_string(), value);

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

    let views = resolve_target_views(pctx, rel_collection, rel_def)?;

    let mut populated = Vec::new();
    for id in &ids {
        if visited.contains(&(rel_collection.to_string(), id.clone())) {
            populated.push(Value::String(id.clone()));
            continue;
        }

        if let Some(target) = resolve_single_target(
            pctx,
            rel_collection,
            rel_def,
            id,
            &views,
            effective_depth,
            visited,
        )? {
            populated.push(document_to_json(&target, Some(rel_collection)));
        }
        // Missing or access-hidden: drop the entry — parity with the top-level
        // has-many path, which never leaves a denied target's raw id in place.
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

    let views = resolve_target_views(pctx, &col, &item_def)?;

    // Resolve to the embedded target, or `null` when missing or access-hidden —
    // parity with the non-poly nested has-one and the top-level poly path, which
    // never leave a denied target's raw `collection/id` ref in place.
    let value = match resolve_single_target(
        pctx,
        &col,
        &item_def,
        &id,
        &views,
        effective_depth,
        visited,
    )? {
        Some(target) => document_to_json(&target, Some(&col)),
        None => Value::Null,
    };
    map.insert(name.to_string(), value);

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

    // Resolve each target collection's view access (read + draft) once.
    let mut views_by: HashMap<String, TargetViews> = HashMap::new();

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

        if !views_by.contains_key(&col) {
            let resolved = resolve_target_views(pctx, &col, &item_def)?;
            views_by.insert(col.clone(), resolved);
        }
        let views = &views_by[&col];

        if let Some(target) =
            resolve_single_target(pctx, &col, &item_def, &id, views, effective_depth, visited)?
        {
            populated.push(document_to_json(&target, Some(&col)));
        }
        // Missing or access-hidden: drop the entry — parity with the non-poly
        // nested has-many and the top-level poly path; never leave a denied
        // target's raw `collection/id` ref in place.
    }
    map.insert(name.to_string(), Value::Array(populated));
    Ok(())
}
