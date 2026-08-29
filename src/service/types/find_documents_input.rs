//! Input for `find_documents` — paginated query with filters.
//!
//! Carries only genuine per-call data. Infrastructure (registry, populate
//! cache, singleflight) lives on the `ServiceContext` — set in one shot by
//! `ServiceContextBuilder::infra` — so an input can never smuggle a stale or
//! missing infra dependency past the context.

use crate::{
    core::Builder,
    db::{FindQuery, LocaleContext},
    service::read::post_process::PostProcessOpts,
};

/// Input for [`find_documents`](crate::service::find_documents).
#[derive(Builder)]
pub struct FindDocumentsInput<'a> {
    #[builder(required)]
    pub query: &'a FindQuery,
    pub depth: i32,
    #[builder(default = true)]
    pub hydrate: bool,
    pub select: Option<&'a [String]>,
    pub locale_ctx: Option<&'a LocaleContext>,
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
    /// Whether cursor-based pagination is enabled (from config).
    /// When true, `PaginationResult` uses cursor mode; when false, page mode.
    pub cursor_enabled: bool,
    /// When true, return only soft-deleted documents (trash view). The service
    /// flips `include_deleted = true` and injects `_deleted_at EXISTS`
    /// post-validation, and routes the access check through `access.trash`.
    /// Callers never push `_deleted_at` into `query.filters` themselves.
    pub trash: bool,
}

impl PostProcessOpts for FindDocumentsInput<'_> {
    fn depth(&self) -> i32 {
        self.depth
    }
    fn include_drafts(&self) -> bool {
        self.include_drafts
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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SAFE-DEFAULT GUARD: a surface that builds `FindDocumentsInput` without
    /// opting in must get the restrictive behavior — drafts hidden, trash
    /// excluded. Flipping either default to `true` would silently leak
    /// unpublished/trashed rows across *every* surface.
    #[test]
    fn builder_defaults_are_restrictive() {
        let fq = FindQuery::default();
        let input = FindDocumentsInput::builder(&fq).build();
        assert!(!input.include_drafts, "drafts must be hidden by default");
        assert!(!input.trash, "trash must be excluded by default");
    }
}
