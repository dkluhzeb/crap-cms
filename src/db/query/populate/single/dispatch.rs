//! Single-document relationship population dispatch.

use anyhow::Result;
use std::collections::HashSet;

use crate::core::cache::CacheBackend;
use crate::core::{
    CollectionDefinition, Document, FieldType, field::flatten_array_sub_fields, upload,
};
use crate::db::query::populate::helpers::{
    TargetViews, cache_or_fetch_doc, fetch_target, target_row_visible,
};
use crate::db::query::populate::{
    CachedDoc, PopulateContext, PopulateCtx, PopulateOpts, Singleflight, locale_cache_key,
    populate_cache_key,
};

use super::{join, nested, nonpoly, poly};

/// Apply the per-request filters to a RAW target document and populate it,
/// returning `None` when the target is hidden by draft visibility or the
/// `read` access decision.
///
/// The shared cache holds only RAW docs (user-independent); draft visibility,
/// the `read` decision, upload sizes, and recursive population are all applied
/// here, per request, so a cached raw doc can never leak one user's filtered
/// view to another. Access constraints are matched against the RAW fields,
/// before population turns relationship fields into objects.
///
/// `views` is the target collection's resolved view access (`read` + `draft`),
/// resolved once by the caller for the whole field (it is collection-level, not
/// per-row).
///
/// # Errors
///
/// Propagates a backend error from the recursive population step.
pub(super) fn finalize_target(
    ctx: &PopulateCtx<'_>,
    collection: &str,
    def: &CollectionDefinition,
    mut raw: Document,
    views: &TargetViews,
    effective_depth: i32,
    visited: &mut HashSet<(String, String)>,
) -> Result<Option<Document>> {
    if !target_row_visible(views, &raw, ctx.published_only, def) {
        return Ok(None);
    }

    if let Some(uc) = &def.upload
        && uc.enabled
    {
        upload::assemble_sizes_object(&mut raw, uc);
    }

    // Recurse, forwarding the access checker + user so nested targets are gated
    // too, and the shared singleflight so nested raw fetches dedup across requests.
    // `effective_depth` is the field's cap (may be shallower than `ctx`'s).
    populate_relationships_cached_with_singleflight(
        &PopulateContext {
            conn: ctx.conn,
            registry: ctx.registry,
            collection_slug: collection,
            def,
        },
        &mut raw,
        visited,
        &PopulateOpts {
            depth: effective_depth - 1,
            select: None,
            locale_ctx: ctx.locale_ctx,
            published_only: ctx.published_only,
            join_access: ctx.join_access,
            user: ctx.user,
        },
        ctx.cache,
        ctx.singleflight,
    )?;

    Ok(Some(raw))
}

/// Resolve one relationship target by id: fetch its RAW doc (shared cache, else
/// singleflight), then [`finalize_target`]. Used by the single-fetch paths
/// (has-one, polymorphic, nested); the batch has-many path fetches raw docs in
/// one query and calls [`finalize_target`] directly.
///
/// # Errors
///
/// Propagates a backend error from the recursive population step.
pub(super) fn resolve_single_target(
    ctx: &PopulateCtx<'_>,
    collection: &str,
    def: &CollectionDefinition,
    id: &str,
    views: &TargetViews,
    effective_depth: i32,
    visited: &mut HashSet<(String, String)>,
) -> Result<Option<Document>> {
    let locale = locale_cache_key(ctx.locale_ctx);
    let key = populate_cache_key(collection, id, locale.as_deref());

    let Some(raw) = cache_or_fetch_doc(ctx.cache, ctx.singleflight, &key, || {
        fetch_target(ctx, collection, def, id)
    })?
    else {
        return Ok(None);
    };

    finalize_target(ctx, collection, def, raw, views, effective_depth, visited)
}

/// Recursively populate relationship fields with full document objects.
///
/// depth=0 is a no-op. Tracks visited (collection, id) pairs to break cycles.
/// Uses a shared `cache` to avoid redundant fetches within the same request.
pub(crate) fn populate_relationships_cached(
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
///
/// # Errors
///
/// Returns a backend error if any relationship-target lookup fails.
pub fn populate_relationships_cached_with_singleflight(
    ctx: &PopulateContext<'_>,
    doc: &mut Document,
    visited: &mut HashSet<(String, String)>,
    opts: &PopulateOpts<'_>,
    cache: &dyn CacheBackend,
    singleflight: &Singleflight<CachedDoc>,
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
    singleflight: &Singleflight<CachedDoc>,
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

    // The doc's own id anchors any reverse-lookup join nested in its containers;
    // clone it to a local so the context can hold it while `doc` is borrowed mut.
    let root_id = doc.id.to_string();
    let nested_pctx = PopulateCtx {
        conn: ctx.conn,
        registry: ctx.registry,
        effective_depth: opts.depth,
        root_id: &root_id,
        locale_ctx: opts.locale_ctx,
        published_only: opts.published_only,
        cache,
        singleflight,
        join_access: opts.join_access,
        user: opts.user,
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
    singleflight: &Singleflight<CachedDoc>,
    visited: &mut HashSet<(String, String)>,
) -> Result<()> {
    // Anchors a reverse-join nested under a flattened array sub-field; unused by
    // the flat relationship/upload paths themselves.
    let root_id = doc.id.to_string();

    for field in flatten_array_sub_fields(&ctx.def.fields) {
        if field.field_type != FieldType::Relationship && field.field_type != FieldType::Upload {
            continue;
        }

        if let Some(sel) = opts.select
            && !sel.iter().any(|s| s == &field.name)
        {
            continue;
        }

        let Some(rel) = &field.relationship else {
            continue;
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
            root_id: &root_id,
            locale_ctx: opts.locale_ctx,
            published_only: opts.published_only,
            cache,
            singleflight,
            join_access: opts.join_access,
            user: opts.user,
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
