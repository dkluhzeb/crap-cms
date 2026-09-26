//! Convenience wrappers that default to a no-op cache.

use anyhow::Result;
use std::collections::HashSet;

use crate::core::cache::NoneCache;
use crate::core::{Document, JoinConfig, Registry};
use crate::db::DbConnection;

use super::{
    PopulateContext, PopulateCtx, PopulateOpts, Singleflight, join::fetch_join_children,
    populate_relationships_batch_cached, populate_relationships_cached,
};

/// Recursively populate relationship fields with full document objects.
/// Convenience wrapper that creates a fresh no-op cache per call.
///
/// # Errors
///
/// Returns a backend error if any relationship-target lookup fails.
pub fn populate_relationships(
    ctx: &PopulateContext<'_>,
    doc: &mut Document,
    visited: &mut HashSet<(String, String)>,
    opts: &PopulateOpts<'_>,
) -> Result<()> {
    populate_relationships_cached(ctx, doc, visited, opts, &NoneCache)
}

/// Batch-populate relationship fields across a slice of documents.
/// Convenience wrapper that creates a fresh no-op cache per call.
///
/// # Errors
///
/// Returns a backend error if any relationship-target lookup fails.
pub fn populate_relationships_batch(
    ctx: &PopulateContext<'_>,
    docs: &mut [Document],
    opts: &PopulateOpts<'_>,
) -> Result<()> {
    populate_relationships_batch_cached(ctx, docs, opts, &NoneCache)
}

/// The children `join` lists for the document `parent_id` outside a populate
/// pass — the admin edit form's join field: the very lookup a read populates
/// the join with (views, draft view, `on` readability, `limit`), with the
/// children themselves left unpopulated. `opts.depth` and `opts.select` are
/// not used. Empty when the join's target collection is not registered.
///
/// # Errors
///
/// Propagates an access-hook or backend error.
pub fn join_children(
    conn: &dyn DbConnection,
    registry: &Registry,
    (join, parent_id): (&JoinConfig, &str),
    opts: &PopulateOpts<'_>,
) -> Result<Vec<Document>> {
    let Some(target_def) = registry.get_collection(&join.collection) else {
        return Ok(Vec::new());
    };

    let singleflight = Singleflight::new();
    let pctx = PopulateCtx {
        conn,
        registry,
        effective_depth: 0,
        root_id: parent_id,
        locale_ctx: opts.locale_ctx,
        published_only: opts.published_only,
        cache: &NoneCache,
        singleflight: &singleflight,
        join_access: opts.join_access,
        user: opts.user,
    };

    let mut listed = fetch_join_children(&pctx, join, target_def, vec![parent_id.to_string()])?;

    Ok(listed.remove(parent_id).unwrap_or_default())
}
