//! Single-document relationship population dispatch.

use anyhow::Result;
use std::collections::HashSet;

use crate::core::cache::CacheBackend;
use crate::core::{Document, FieldType, field::flatten_array_sub_fields};
use crate::db::query::populate::{PopulateContext, PopulateCtx, PopulateOpts, Singleflight};

use super::{join, nested, nonpoly, poly};

/// Recursively populate relationship fields with full document objects.
///
/// depth=0 is a no-op. Tracks visited (collection, id) pairs to break cycles.
/// Uses a shared `cache` to avoid redundant fetches within the same request.
pub fn populate_relationships_cached(
    ctx: &PopulateContext<'_>,
    doc: &mut Document,
    visited: &mut HashSet<(String, String)>,
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
) -> Result<()> {
    let singleflight = Singleflight::new();

    populate_relationships_cached_inner(ctx, doc, visited, opts, cache, &singleflight)
}

/// Variant of [`populate_relationships_cached`] that accepts an externally
/// owned singleflight so concurrent populate trees across requests can
/// deduplicate cache-miss DB fetches for the same target document.
///
/// Callers in the service layer pass the process-wide
/// [`SharedPopulateSingleflight`](crate::db::query::SharedPopulateSingleflight)
/// here. Internal callers keep using the fresh-per-call variant above.
pub fn populate_relationships_cached_with_singleflight(
    ctx: &PopulateContext<'_>,
    doc: &mut Document,
    visited: &mut HashSet<(String, String)>,
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
    singleflight: &Singleflight<Option<Document>>,
) -> Result<()> {
    populate_relationships_cached_inner(ctx, doc, visited, opts, cache, singleflight)
}

/// Internal entry point that carries an explicit singleflight, so recursive
/// calls within the same populate tree share the same dedup table.
pub(crate) fn populate_relationships_cached_inner(
    ctx: &PopulateContext<'_>,
    doc: &mut Document,
    visited: &mut HashSet<(String, String)>,
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
    singleflight: &Singleflight<Option<Document>>,
) -> Result<()> {
    if opts.depth <= 0 {
        return Ok(());
    }

    let visit_key = (ctx.collection_slug.to_string(), doc.id.to_string());

    if visited.contains(&visit_key) {
        return Ok(());
    }

    visited.insert(visit_key);

    populate_flat_relationships(ctx, doc, opts, cache, singleflight, visited)?;

    let nested_pctx = PopulateCtx {
        conn: ctx.conn,
        registry: ctx.registry,
        effective_depth: opts.depth,
        locale_ctx: opts.locale_ctx,
        cache,
        singleflight,
    };

    nested::populate_containers_in_doc(&nested_pctx, doc, &ctx.def.fields, visited)?;

    join::populate_join_fields(ctx, doc, visited, opts, cache)?;

    Ok(())
}

/// Populate non-join relationship/upload fields on a single document.
fn populate_flat_relationships(
    ctx: &PopulateContext<'_>,
    doc: &mut Document,
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
    singleflight: &Singleflight<Option<Document>>,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    for field in flatten_array_sub_fields(&ctx.def.fields) {
        if field.field_type != FieldType::Relationship && field.field_type != FieldType::Upload {
            continue;
        }

        if let Some(sel) = opts.select
            && !sel.iter().any(|s| s == &field.name)
        {
            continue;
        }

        let rel = match &field.relationship {
            Some(rc) => rc,
            None => continue,
        };

        let effective_depth = match rel.max_depth {
            Some(max) if max < opts.depth => max,
            _ => opts.depth,
        };

        if effective_depth <= 0 {
            continue;
        }

        let pctx = PopulateCtx {
            conn: ctx.conn,
            registry: ctx.registry,
            effective_depth,
            locale_ctx: opts.locale_ctx,
            cache,
            singleflight,
        };

        if rel.is_polymorphic() {
            if rel.has_many {
                poly::populate_poly_has_many(&pctx, doc, &field.name, visited)?;
            } else {
                poly::populate_poly_has_one(&pctx, doc, &field.name, visited)?;
            }
        } else {
            let rel_def = match ctx.registry.get_collection(&rel.collection) {
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

    Ok(())
}
