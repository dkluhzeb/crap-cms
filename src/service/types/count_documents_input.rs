//! Input for `count_documents` — document counting with filters.

use crate::db::{FilterClause, LocaleContext};

/// Input for [`count_documents`](crate::service::count_documents).
///
/// Mirrors [`FindDocumentsInput`](crate::service::FindDocumentsInput) — callers
/// supply user filters plus the typed flags (`trash`, `include_drafts`) and the
/// service injects the matching system filters post-validation.
pub struct CountDocumentsInput<'a> {
    pub filters: &'a [FilterClause],
    pub locale_ctx: Option<&'a LocaleContext>,
    pub search: Option<&'a str>,
    /// When `true`, count only soft-deleted documents (trash view). The service
    /// flips `include_deleted = true` and injects `_deleted_at EXISTS`.
    pub trash: bool,
    /// When `true`, the caller wants drafts included. When `false` (default)
    /// and the collection has drafts, the service injects
    /// `_status = "published"` post-validation.
    pub include_drafts: bool,
}

impl<'a> CountDocumentsInput<'a> {
    pub fn builder(filters: &'a [FilterClause]) -> CountDocumentsInputBuilder<'a> {
        CountDocumentsInputBuilder::new(filters)
    }
}

/// Builder for [`CountDocumentsInput`].
pub struct CountDocumentsInputBuilder<'a> {
    filters: &'a [FilterClause],
    locale_ctx: Option<&'a LocaleContext>,
    search: Option<&'a str>,
    trash: bool,
    include_drafts: bool,
}

impl<'a> CountDocumentsInputBuilder<'a> {
    pub fn new(filters: &'a [FilterClause]) -> Self {
        Self {
            filters,
            locale_ctx: None,
            search: None,
            trash: false,
            include_drafts: false,
        }
    }

    pub fn locale_ctx(mut self, locale_ctx: Option<&'a LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;
        self
    }

    pub fn search(mut self, search: Option<&'a str>) -> Self {
        self.search = search;
        self
    }

    pub fn trash(mut self, trash: bool) -> Self {
        self.trash = trash;
        self
    }

    pub fn include_drafts(mut self, include_drafts: bool) -> Self {
        self.include_drafts = include_drafts;
        self
    }

    pub fn build(self) -> CountDocumentsInput<'a> {
        CountDocumentsInput {
            filters: self.filters,
            locale_ctx: self.locale_ctx,
            search: self.search,
            trash: self.trash,
            include_drafts: self.include_drafts,
        }
    }
}
