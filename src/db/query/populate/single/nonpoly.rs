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

        // DB miss (truly missing): omit from the array. A copy, so a target
        // referenced twice in one list is embedded twice.
        let Some(raw) = raws.get(id).cloned() else {
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
            populated.push(document_to_json(&target, Some(rel_collection)));
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
                document_to_json(&target, Some(rel_collection)),
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
mod tests;
