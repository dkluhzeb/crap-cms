//! Pagination context — `{{pagination.*}}` for list pages.
//!
//! Two modes coexist: page-mode (numeric page, total_pages) and cursor-mode
//! (no page numbers, just prev/next URLs). Cursor mode omits `page` and
//! `total_pages` so templates can detect mode via key presence.

use serde::Serialize;

use crate::db::query::PaginationResult;

/// Pagination metadata for list views.
#[derive(Serialize)]
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
