//! Read options shared across all read operations.

use crate::{
    core::{Document, Registry, cache::CacheBackend},
    db::{FilterClause, LocaleContext},
};

/// Options controlling read behavior and post-processing.
pub struct ReadOptions<'a> {
    /// Relationship population depth (0 = skip).
    pub depth: i32,
    /// Whether to hydrate join-table data (arrays, blocks, has-many).
    /// Ignored by `find_document_by_id` which uses `ops::find_by_id_full` (handles its own hydration).
    pub hydrate: bool,
    /// Optional field selection filter.
    pub select: Option<&'a [String]>,
    /// Locale context for localized queries.
    pub locale_ctx: Option<&'a LocaleContext>,
    /// Registry for relationship population.
    pub registry: Option<&'a Registry>,
    /// Authenticated user (for field-level access + hook context).
    pub user: Option<&'a Document>,
    /// UI locale (for hook context).
    pub ui_locale: Option<&'a str>,
    /// Whether to overlay draft version data (find_by_id only).
    pub use_draft: bool,
    /// Access constraint filters for find_by_id (pre-computed by caller).
    pub access_constraints: Option<Vec<FilterClause>>,
    /// Optional cache backend for relationship population.
    pub cache: Option<&'a dyn CacheBackend>,
}

impl Default for ReadOptions<'_> {
    fn default() -> Self {
        Self {
            depth: 0,
            hydrate: true,
            select: None,
            locale_ctx: None,
            registry: None,
            user: None,
            ui_locale: None,
            use_draft: false,
            access_constraints: None,
            cache: None,
        }
    }
}
