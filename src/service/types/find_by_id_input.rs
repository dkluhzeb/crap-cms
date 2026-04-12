//! Input for `find_document_by_id` — single document lookup.

use crate::{
    core::{Registry, cache::CacheBackend},
    db::{FilterClause, LocaleContext},
    service::read::post_process::PostProcessOpts,
};

/// Input for [`find_document_by_id`](crate::service::find_document_by_id).
pub struct FindByIdInput<'a> {
    pub id: &'a str,
    pub depth: i32,
    pub select: Option<&'a [String]>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub registry: Option<&'a Registry>,
    pub cache: Option<&'a dyn CacheBackend>,
    pub use_draft: bool,
    pub access_constraints: Option<Vec<FilterClause>>,
    /// When true, include soft-deleted documents (trash view).
    pub include_deleted: bool,
}

impl<'a> FindByIdInput<'a> {
    pub fn builder(id: &'a str) -> FindByIdInputBuilder<'a> {
        FindByIdInputBuilder::new(id)
    }
}

/// Builder for [`FindByIdInput`].
pub struct FindByIdInputBuilder<'a> {
    id: &'a str,
    depth: i32,
    select: Option<&'a [String]>,
    locale_ctx: Option<&'a LocaleContext>,
    registry: Option<&'a Registry>,
    cache: Option<&'a dyn CacheBackend>,
    use_draft: bool,
    access_constraints: Option<Vec<FilterClause>>,
    include_deleted: bool,
}

impl<'a> FindByIdInputBuilder<'a> {
    pub fn new(id: &'a str) -> Self {
        Self {
            id,
            depth: 0,
            select: None,
            locale_ctx: None,
            registry: None,
            cache: None,
            use_draft: false,
            access_constraints: None,
            include_deleted: false,
        }
    }

    pub fn depth(mut self, depth: i32) -> Self {
        self.depth = depth;
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

    pub fn include_deleted(mut self, include_deleted: bool) -> Self {
        self.include_deleted = include_deleted;
        self
    }

    pub fn build(self) -> FindByIdInput<'a> {
        FindByIdInput {
            id: self.id,
            depth: self.depth,
            select: self.select,
            locale_ctx: self.locale_ctx,
            registry: self.registry,
            cache: self.cache,
            use_draft: self.use_draft,
            access_constraints: self.access_constraints,
            include_deleted: self.include_deleted,
        }
    }
}

impl PostProcessOpts for FindByIdInput<'_> {
    fn depth(&self) -> i32 {
        self.depth
    }
    fn hydrate(&self) -> bool {
        false
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
