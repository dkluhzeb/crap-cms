//! Single-document relationship population (recursive, cached).

mod join;
mod nonpoly;
mod poly;
#[cfg(test)]
mod tests;

use anyhow::Result;
use std::collections::HashSet;

use crate::core::{Document, FieldType};
use crate::db::query::populate::{PopulateCache, PopulateContext, PopulateCtx, PopulateOpts};

/// Recursively populate relationship fields with full document objects.
/// depth=0 is a no-op. Tracks visited (collection, id) pairs to break cycles.
/// If `select` is provided, only populate relationship fields in the select list.
/// Uses a shared `cache` to avoid redundant fetches within the same request.
pub fn populate_relationships_cached(
    ctx: &PopulateContext<'_>,
    doc: &mut Document,
    visited: &mut HashSet<(String, String)>,
    opts: &PopulateOpts<'_>,
    cache: &PopulateCache,
) -> Result<()> {
    let conn = ctx.conn;
    let registry = ctx.registry;
    let collection_slug = ctx.collection_slug;
    let def = ctx.def;
    let depth = opts.depth;
    let select = opts.select;
    let locale_ctx = opts.locale_ctx;

    if depth <= 0 {
        return Ok(());
    }

    let visit_key = (collection_slug.to_string(), doc.id.to_string());

    if visited.contains(&visit_key) {
        return Ok(());
    }
    visited.insert(visit_key);

    for field in &def.fields {
        if field.field_type != FieldType::Relationship && field.field_type != FieldType::Upload {
            continue;
        }
        // Skip populating fields not in the select list
        if let Some(sel) = select
            && !sel.iter().any(|s| s == &field.name)
        {
            continue;
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

        let pctx = PopulateCtx {
            conn,
            registry,
            effective_depth,
            locale_ctx,
            cache,
        };

        if rel.is_polymorphic() {
            // Polymorphic: values are "collection/id" composite strings
            if rel.has_many {
                poly::populate_poly_has_many(&pctx, doc, &field.name, visited)?;
            } else {
                poly::populate_poly_has_one(&pctx, doc, &field.name, visited)?;
            }
        } else {
            // Non-polymorphic: look up the target collection definition
            let rel_def = match registry.get_collection(&rel.collection) {
                Some(d) => d.clone(),
                None => continue,
            };

            if rel.has_many {
                nonpoly::populate_nonpoly_has_many(
                    &pctx,
                    doc,
                    &field.name,
                    &rel.collection,
                    &rel_def,
                    visited,
                )?;
            } else {
                nonpoly::populate_nonpoly_has_one(
                    &pctx,
                    doc,
                    &field.name,
                    &rel.collection,
                    &rel_def,
                    visited,
                )?;
            }
        }
    }

    // Join fields: virtual reverse lookups
    join::populate_join_fields(ctx, doc, visited, opts, cache)?;

    Ok(())
}
