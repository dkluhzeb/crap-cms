//! Input for `find_documents` — paginated query with filters.

use crate::{
    core::{Registry, cache::CacheBackend},
    db::{FilterClause, FindQuery, LocaleContext, query::SharedPopulateSingleflight},
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
    /// When `true`, the caller wants drafts in the result set, so the service
    /// does NOT inject `_status = "published"`. When `false` (default) and the
    /// collection has drafts, the service injects the published-only filter
    /// post-validation. Callers never push `_status` into `query.filters`
    /// themselves.
    pub include_drafts: bool,
    /// Optional explicit `_status` filter values for the admin list page's
    /// filter builder. Mutually exclusive with the
    /// `include_drafts = false` default-published injection — when set,
    /// the service treats it like an `include_drafts = true` request and
    /// *additionally* pins the query to documents whose `_status` matches
    /// one of the supplied values. One value → `_status = X`, multiple
    /// values → `_status IN (X, Y, …)`. Translated from
    /// `?where[_status][equals]=X` URL filters (top-level *and* OR-bucket
    /// forms) by the admin list handler. `parse_where_params` and
    /// `validate_user_filters` continue to reject `_status` in the
    /// generic-user-filter path; this typed param is the supported entry
    /// point.
    pub status_filter: Option<Vec<String>>,
    pub access_constraints: Option<Vec<FilterClause>>,
    /// Whether cursor-based pagination is enabled (from config).
    /// When true, PaginationResult uses cursor mode; when false, page mode.
    pub cursor_enabled: bool,
    /// When true, return only soft-deleted documents (trash view). The service
    /// flips `include_deleted = true` and injects `_deleted_at EXISTS`
    /// post-validation, and routes the access check through `access.trash`.
    /// Callers never push `_deleted_at` into `query.filters` themselves.
    pub trash: bool,
    /// Optional process-wide singleflight for deduplicating concurrent
    /// populate cache-miss fetches across requests. When `None`, the service
    /// layer falls back to a fresh per-call singleflight (dedup still works
    /// within a single populate tree, but not across concurrent populates).
    pub singleflight: Option<SharedPopulateSingleflight>,
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
    include_drafts: bool,
    status_filter: Option<Vec<String>>,
    access_constraints: Option<Vec<FilterClause>>,
    cursor_enabled: bool,
    trash: bool,
    singleflight: Option<SharedPopulateSingleflight>,
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
            include_drafts: false,
            status_filter: None,
            access_constraints: None,
            cursor_enabled: false,
            trash: false,
            singleflight: None,
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

    pub fn include_drafts(mut self, include_drafts: bool) -> Self {
        self.include_drafts = include_drafts;
        self
    }

    pub fn status_filter(mut self, status_filter: Option<Vec<String>>) -> Self {
        self.status_filter = status_filter;
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

    /// Attach a process-wide singleflight so populate cache-miss fetches
    /// dedup across concurrent requests.
    pub fn singleflight(mut self, singleflight: Option<SharedPopulateSingleflight>) -> Self {
        self.singleflight = singleflight;
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
            include_drafts: self.include_drafts,
            status_filter: self.status_filter,
            access_constraints: self.access_constraints,
            cursor_enabled: self.cursor_enabled,
            trash: self.trash,
            singleflight: self.singleflight,
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
    fn singleflight(&self) -> Option<&SharedPopulateSingleflight> {
        self.singleflight.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    use crate::db::query::Singleflight;

    /// Regression: `FindDocumentsInput::builder().singleflight(..)` must plumb
    /// the Arc through to the built input so post-processing can share one
    /// singleflight across concurrent populates.
    #[test]
    fn builder_threads_singleflight_through() {
        let fq = FindQuery::default();
        let sf: SharedPopulateSingleflight = Arc::new(Singleflight::new());
        let before = Arc::strong_count(&sf);

        let input = FindDocumentsInput::builder(&fq)
            .singleflight(Some(sf.clone()))
            .build();

        assert!(input.singleflight.is_some());
        // Arc::clone bumped strong count by 1 (builder) then moved into input.
        assert_eq!(Arc::strong_count(&sf), before + 1);

        // PostProcessOpts hands out a borrow of the same Arc.
        let via_trait = PostProcessOpts::singleflight(&input).expect("singleflight present");
        assert!(Arc::ptr_eq(via_trait, &sf));
    }

    #[test]
    fn builder_singleflight_defaults_to_none() {
        let fq = FindQuery::default();
        let input = FindDocumentsInput::builder(&fq).build();
        assert!(input.singleflight.is_none());
        assert!(PostProcessOpts::singleflight(&input).is_none());
    }
}
