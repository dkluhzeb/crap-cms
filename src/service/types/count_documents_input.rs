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
    /// Explicit `_status` view filter (e.g. `["draft"]`), mirroring
    /// [`FindDocumentsInput::status_filter`]. Passed to `requested_views` so a
    /// status-filtered count scopes to the same views as the matching `find`
    /// — otherwise the reported total would not match the filtered rows.
    pub status_filter: Option<Vec<String>>,
}

impl<'a> CountDocumentsInput<'a> {
    #[must_use]
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
    status_filter: Option<Vec<String>>,
}

impl<'a> CountDocumentsInputBuilder<'a> {
    pub fn new(filters: &'a [FilterClause]) -> Self {
        Self {
            filters,
            locale_ctx: None,
            search: None,
            trash: false,
            include_drafts: false,
            status_filter: None,
        }
    }

    pub fn status_filter(mut self, status_filter: Option<Vec<String>>) -> Self {
        self.status_filter = status_filter;
        self
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
            status_filter: self.status_filter,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults_are_off() {
        let f: &[FilterClause] = &[];
        let c = CountDocumentsInput::builder(f).build();
        assert!(c.locale_ctx.is_none());
        assert!(c.search.is_none());
        assert!(!c.trash);
        assert!(!c.include_drafts);
    }

    /// `trash` and `include_drafts` set to distinct values — a swap in
    /// `build()` would surface here.
    #[test]
    fn builder_wires_distinct_flags() {
        let f: &[FilterClause] = &[];
        let c = CountDocumentsInput::builder(f)
            .search(Some("hello"))
            .trash(true)
            .include_drafts(false)
            .status_filter(Some(vec!["draft".to_string()]))
            .build();
        assert_eq!(c.search, Some("hello"));
        assert!(c.trash);
        assert!(!c.include_drafts);
        assert_eq!(c.status_filter.as_deref(), Some(&["draft".to_string()][..]));
    }
}
