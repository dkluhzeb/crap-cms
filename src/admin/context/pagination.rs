//! Pagination context — `{{pagination.*}}` for list pages.
//!
//! Two modes coexist: page-mode (numeric page, `total_pages`) and cursor-mode
//! (no page numbers, just prev/next URLs). Cursor mode omits `page` and
//! `total_pages` so templates can detect mode via key presence.

use schemars::JsonSchema;
use serde::Serialize;

use crate::db::PaginationResult;

/// Pagination metadata for list views.
#[derive(Serialize, JsonSchema)]
pub struct PaginationContext {
    pub per_page: i64,
    pub total: i64,
    pub has_prev: bool,
    pub has_next: bool,
    pub prev_url: String,
    pub next_url: String,
    /// Page-mode only — current page number (1-indexed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    /// Page-mode only — total page count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_pages: Option<i64>,
}

impl PaginationContext {
    /// Build from a [`PaginationResult`] plus pre-computed prev/next URLs.
    pub fn from_result(pr: &PaginationResult, prev_url: String, next_url: String) -> Self {
        let (page, total_pages) = match pr.page {
            Some(p) => (Some(p), Some(pr.total_pages.unwrap_or(0))),
            None => (None, None),
        };

        Self {
            per_page: pr.limit,
            total: pr.total_docs,
            has_prev: pr.has_prev_page,
            has_next: pr.has_next_page,
            prev_url,
            next_url,
            page,
            total_pages,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(page: Option<i64>, total_pages: Option<i64>) -> PaginationResult {
        PaginationResult {
            total_docs: 42,
            limit: 10,
            has_next_page: true,
            has_prev_page: false,
            total_pages,
            page,
            page_start: None,
            prev_page: None,
            next_page: None,
            start_cursor: None,
            end_cursor: None,
        }
    }

    #[test]
    fn page_mode_carries_page_and_total_pages() {
        let ctx = PaginationContext::from_result(
            &result(Some(2), Some(5)),
            "/prev".into(),
            "/next".into(),
        );
        assert_eq!(ctx.per_page, 10);
        assert_eq!(ctx.total, 42);
        assert!(!ctx.has_prev);
        assert!(ctx.has_next);
        assert_eq!(ctx.prev_url, "/prev");
        assert_eq!(ctx.next_url, "/next");
        assert_eq!(ctx.page, Some(2));
        assert_eq!(ctx.total_pages, Some(5));
    }

    #[test]
    fn cursor_mode_omits_page_and_total_pages() {
        let ctx = PaginationContext::from_result(&result(None, None), String::new(), String::new());
        assert_eq!(ctx.page, None);
        assert_eq!(ctx.total_pages, None);
    }

    #[test]
    fn page_mode_defaults_missing_total_pages_to_zero() {
        let ctx =
            PaginationContext::from_result(&result(Some(1), None), String::new(), String::new());
        assert_eq!(ctx.total_pages, Some(0));
    }
}
