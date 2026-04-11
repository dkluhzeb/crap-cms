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

impl<'a> ReadOptions<'a> {
    /// Create a builder with all fields defaulted.
    pub fn builder() -> ReadOptionsBuilder<'a> {
        ReadOptionsBuilder::new()
    }
}

/// Builder for [`ReadOptions`]. Created via [`ReadOptions::builder`].
pub struct ReadOptionsBuilder<'a> {
    pub(in crate::service) depth: i32,
    pub(in crate::service) hydrate: bool,
    pub(in crate::service) select: Option<&'a [String]>,
    pub(in crate::service) locale_ctx: Option<&'a LocaleContext>,
    pub(in crate::service) registry: Option<&'a Registry>,
    pub(in crate::service) user: Option<&'a Document>,
    pub(in crate::service) ui_locale: Option<&'a str>,
    pub(in crate::service) use_draft: bool,
    pub(in crate::service) access_constraints: Option<Vec<FilterClause>>,
    pub(in crate::service) cache: Option<&'a dyn CacheBackend>,
}

impl Default for ReadOptionsBuilder<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> ReadOptionsBuilder<'a> {
    pub fn new() -> Self {
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

    pub fn depth(mut self, depth: i32) -> Self {
        self.depth = depth;
        self
    }

    pub fn hydrate(mut self, hydrate: bool) -> Self {
        self.hydrate = hydrate;
        self
    }

    pub fn select(mut self, select: Option<&'a [String]>) -> Self {
        self.select = select;
        self
    }

    pub fn locale_ctx(mut self, locale_ctx: Option<&'a LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;
        self
    }

    pub fn registry(mut self, registry: Option<&'a Registry>) -> Self {
        self.registry = registry;
        self
    }

    pub fn user(mut self, user: Option<&'a Document>) -> Self {
        self.user = user;
        self
    }

    pub fn ui_locale(mut self, ui_locale: Option<&'a str>) -> Self {
        self.ui_locale = ui_locale;
        self
    }

    pub fn use_draft(mut self, use_draft: bool) -> Self {
        self.use_draft = use_draft;
        self
    }

    pub fn access_constraints(mut self, access_constraints: Option<Vec<FilterClause>>) -> Self {
        self.access_constraints = access_constraints;
        self
    }

    pub fn cache(mut self, cache: Option<&'a dyn CacheBackend>) -> Self {
        self.cache = cache;
        self
    }

    pub fn build(self) -> ReadOptions<'a> {
        ReadOptions {
            depth: self.depth,
            hydrate: self.hydrate,
            select: self.select,
            locale_ctx: self.locale_ctx,
            registry: self.registry,
            user: self.user,
            ui_locale: self.ui_locale,
            use_draft: self.use_draft,
            access_constraints: self.access_constraints,
            cache: self.cache,
        }
    }
}
