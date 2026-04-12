//! Input for `find_documents` — paginated query with filters.

use crate::{
    core::{Registry, cache::CacheBackend},
    db::{FilterClause, FindQuery, LocaleContext},
    service::read::post_process::PostProcessOpts,
};

/// Input for [`find_documents`](crate::service::find_documents).
pub struct FindDocumentsInput<'a> {
    pub query: &'a FindQuery,
    pub depth: i32,
    pub hydrate: bool,
    pub select: Option<&'a [String]>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub registry: Option<&'a Registry>,
    pub cache: Option<&'a dyn CacheBackend>,
    pub use_draft: bool,
    pub access_constraints: Option<Vec<FilterClause>>,
    /// Whether cursor-based pagination is enabled (from config).
    /// When true, PaginationResult uses cursor mode; when false, page mode.
    pub cursor_enabled: bool,
    /// When true, use `access.trash` (or fallback `access.update`) instead of
    /// `access.read` for the access check. Used for trash/soft-delete queries.
    pub trash: bool,
}

impl<'a> FindDocumentsInput<'a> {
    pub fn builder(query: &'a FindQuery) -> FindDocumentsInputBuilder<'a> {
        FindDocumentsInputBuilder::new(query)
    }
}

/// Builder for [`FindDocumentsInput`].
pub struct FindDocumentsInputBuilder<'a> {
    query: &'a FindQuery,
    depth: i32,
    hydrate: bool,
    select: Option<&'a [String]>,
    locale_ctx: Option<&'a LocaleContext>,
    registry: Option<&'a Registry>,
    cache: Option<&'a dyn CacheBackend>,
    use_draft: bool,
    access_constraints: Option<Vec<FilterClause>>,
    cursor_enabled: bool,
    trash: bool,
}

impl<'a> FindDocumentsInputBuilder<'a> {
    pub fn new(query: &'a FindQuery) -> Self {
        Self {
            query,
            depth: 0,
            hydrate: true,
            select: None,
            locale_ctx: None,
            registry: None,
            cache: None,
            use_draft: false,
            access_constraints: None,
            cursor_enabled: false,
            trash: false,
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

    pub fn cache(mut self, cache: Option<&'a dyn CacheBackend>) -> Self {
        self.cache = cache;
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

    pub fn cursor_enabled(mut self, cursor_enabled: bool) -> Self {
        self.cursor_enabled = cursor_enabled;
        self
    }

    pub fn trash(mut self, trash: bool) -> Self {
        self.trash = trash;
        self
    }

    pub fn build(self) -> FindDocumentsInput<'a> {
        FindDocumentsInput {
            query: self.query,
            depth: self.depth,
            hydrate: self.hydrate,
            select: self.select,
            locale_ctx: self.locale_ctx,
            registry: self.registry,
            cache: self.cache,
            use_draft: self.use_draft,
            access_constraints: self.access_constraints,
            cursor_enabled: self.cursor_enabled,
            trash: self.trash,
        }
    }
}

impl PostProcessOpts for FindDocumentsInput<'_> {
    fn depth(&self) -> i32 {
        self.depth
    }
    fn hydrate(&self) -> bool {
        self.hydrate
    }
    fn select(&self) -> Option<&[String]> {
        self.select
    }
    fn locale_ctx(&self) -> Option<&LocaleContext> {
        self.locale_ctx
    }
    fn registry(&self) -> Option<&Registry> {
        self.registry
    }
    fn ui_locale(&self) -> Option<&str> {
        None
    }
    fn cache(&self) -> Option<&dyn CacheBackend> {
        self.cache
    }
}
