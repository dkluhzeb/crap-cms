//! Convenience wrappers that default to a no-op cache.

use anyhow::Result;
use std::collections::HashSet;

use crate::core::Document;
use crate::core::cache::NoneCache;

use super::{
    PopulateContext, PopulateOpts, populate_relationships_batch_cached,
    populate_relationships_cached,
};

/// Recursively populate relationship fields with full document objects.
/// Convenience wrapper that creates a fresh no-op cache per call.
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
pub fn populate_relationships_batch(
    ctx: &PopulateContext<'_>,
    docs: &mut [Document],
    opts: &PopulateOpts<'_>,
) -> Result<()> {
    populate_relationships_batch_cached(ctx, docs, opts, &NoneCache)
}
