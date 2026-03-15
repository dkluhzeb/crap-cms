//! Type definitions for the populate subsystem.

use dashmap::DashMap;

use crate::core::{CollectionDefinition, Document, Registry};
use crate::db::{DbConnection, LocaleContext};

/// Shared cache for populated documents. Key is (collection_slug, document_id).
/// Uses DashMap for concurrent cross-request sharing with interior mutability.
pub type PopulateCache = DashMap<(String, String), Document>;

/// Bundled parameters for inner population helpers, reducing argument count.
///
/// Carries the connection, registry, effective depth, locale context, and cache
/// that every recursive population function needs. The remaining per-call params
/// (doc/docs, field_name, rel_collection, rel_def, visited) stay as regular args.
pub(crate) struct PopulateCtx<'a> {
    pub conn: &'a dyn DbConnection,
    pub registry: &'a Registry,
    pub effective_depth: i32,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub cache: &'a PopulateCache,
}

/// Collection and registry context for population.
pub struct PopulateContext<'a> {
    pub(crate) conn: &'a dyn DbConnection,
    pub(crate) registry: &'a Registry,
    pub(crate) collection_slug: &'a str,
    pub(crate) def: &'a CollectionDefinition,
}

impl<'a> PopulateContext<'a> {
    pub fn new(
        conn: &'a dyn DbConnection,
        registry: &'a Registry,
        collection_slug: &'a str,
        def: &'a CollectionDefinition,
    ) -> Self {
        Self {
            conn,
            registry,
            collection_slug,
            def,
        }
    }
}

/// Options controlling population behavior.
pub struct PopulateOpts<'a> {
    pub(crate) depth: i32,
    pub(crate) select: Option<&'a [String]>,
    pub(crate) locale_ctx: Option<&'a LocaleContext>,
}

impl<'a> PopulateOpts<'a> {
    pub fn new(depth: i32) -> Self {
        Self {
            depth,
            select: None,
            locale_ctx: None,
        }
    }

    pub fn select(mut self, select: &'a [String]) -> Self {
        self.select = Some(select);
        self
    }

    pub fn locale_ctx(mut self, ctx: &'a LocaleContext) -> Self {
        self.locale_ctx = Some(ctx);
        self
    }
}
