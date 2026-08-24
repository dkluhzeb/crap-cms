//! Pagination query parameters and resolved values.

use serde::Deserialize;

/// Query parameters for paginated collection list views.
#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    /// The current page number (1-indexed).
    pub page: Option<i64>,
    /// The number of items per page.
    pub per_page: Option<i64>,
    /// Search query string.
    pub search: Option<String>,
    /// Sort string (e.g. "title" or "-title").
    pub sort: Option<String>,
    /// Forward cursor for cursor-based pagination.
    pub after_cursor: Option<String>,
    /// Backward cursor for cursor-based pagination.
    pub before_cursor: Option<String>,
    /// When "1", show the trash view (soft-deleted documents only).
    pub trash: Option<String>,
}

/// Resolved pagination values ready for use in queries.
pub struct Pagination {
    /// The current page number (1-indexed, clamped to >= 1).
    pub page: i64,
    /// The number of items per page (clamped to config bounds).
    pub per_page: i64,
    /// The offset for SQL queries.
    pub offset: i64,
}

impl PaginationParams {
    /// Resolve raw query parameters into clamped, ready-to-use pagination values.
    pub fn resolve(&self, config: &crate::config::PaginationConfig) -> Pagination {
        let page = self.page.unwrap_or(1).max(1);
        let per_page = config.resolve_limit(self.per_page);
        // Saturating math: an absurd `page` (e.g. `i64::MAX` from the query
        // string) would overflow `(page - 1) * per_page` — a panic under debug
        // overflow checks, a silent wrap in release. Saturate instead, so a
        // past-the-end page just yields a huge offset and an empty result.
        let offset = page.saturating_sub(1).saturating_mul(per_page);

        Pagination {
            page,
            per_page,
            offset,
        }
    }
}

impl Pagination {
    /// Previous page number (1-indexed), clamped to `>= 1` — for a "prev" link.
    /// Page 1's previous is page 1.
    #[must_use]
    pub fn prev_page(&self) -> u64 {
        self.page.saturating_sub(1).max(1).cast_unsigned()
    }

    /// Next page number (1-indexed) — for a "next" link. Saturates rather than
    /// overflowing on an absurd current page (mirrors `resolve`'s offset math).
    #[must_use]
    pub fn next_page(&self) -> u64 {
        self.page.saturating_add(1).cast_unsigned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PaginationConfig, PaginationMode};
    use crate::core::Document;
    use crate::db::PaginationResult;

    fn test_config() -> PaginationConfig {
        PaginationConfig {
            default_limit: 20,
            max_limit: 1000,
            mode: PaginationMode::Page,
        }
    }

    fn params(page: Option<i64>, per_page: Option<i64>) -> PaginationParams {
        PaginationParams {
            page,
            per_page,
            search: None,
            sort: None,
            after_cursor: None,
            before_cursor: None,
            trash: None,
        }
    }

    fn pagination(page: i64) -> Pagination {
        Pagination {
            page,
            per_page: 20,
            offset: 0,
        }
    }

    #[test]
    fn prev_page_clamps_to_one() {
        assert_eq!(pagination(1).prev_page(), 1, "page 1's prev is page 1");
        assert_eq!(pagination(5).prev_page(), 4);
    }

    #[test]
    fn next_page_increments() {
        assert_eq!(pagination(1).next_page(), 2);
        assert_eq!(pagination(5).next_page(), 6);
    }

    #[test]
    fn next_page_saturates_on_overflow() {
        // A colossal current page must not overflow `page + 1` (a debug-mode
        // panic / release-mode wrap). It saturates to `i64::MAX`.
        assert_eq!(pagination(i64::MAX).next_page(), i64::MAX.cast_unsigned());
    }

    #[test]
    fn resolve_defaults_when_unset() {
        let cfg = test_config();
        let p = params(None, None).resolve(&cfg);
        assert_eq!(p.page, 1);
        assert_eq!(p.per_page, 20);
        assert_eq!(p.offset, 0);
    }

    #[test]
    fn resolve_clamps_per_page_zero_to_one() {
        // `per_page = 0` would divide/multiply trivially; clamp raises it to 1
        // to avoid degenerate pagination and division-by-zero downstream.
        let cfg = test_config();
        let p = params(None, Some(0)).resolve(&cfg);
        assert_eq!(p.per_page, 1, "per_page = 0 must clamp to 1");
        assert_eq!(p.offset, 0);
    }

    #[test]
    fn resolve_clamps_per_page_negative_to_one() {
        let cfg = test_config();
        let p = params(None, Some(-5)).resolve(&cfg);
        assert_eq!(p.per_page, 1, "negative per_page must clamp to 1");
    }

    #[test]
    fn resolve_clamps_per_page_above_max() {
        let cfg = test_config();
        let p = params(None, Some(50_000)).resolve(&cfg);
        assert_eq!(
            p.per_page, cfg.max_limit,
            "per_page above max_limit must clamp to max_limit"
        );
    }

    #[test]
    fn resolve_clamps_page_below_one() {
        let cfg = test_config();
        let p = params(Some(0), None).resolve(&cfg);
        assert_eq!(p.page, 1, "page = 0 must clamp to 1");
        assert_eq!(p.offset, 0);
        let p = params(Some(-7), None).resolve(&cfg);
        assert_eq!(p.page, 1, "negative page must clamp to 1");
    }

    #[test]
    fn resolve_offset_past_totals_has_no_effect_on_resolve() {
        // `resolve()` does not know about `total` — it just computes offset = (page - 1) * per_page.
        // A page far beyond the data is legal input; the caller's query returns an empty page.
        let cfg = test_config();
        let p = params(Some(10_000), Some(20)).resolve(&cfg);
        assert_eq!(p.page, 10_000);
        assert_eq!(p.per_page, 20);
        assert_eq!(p.offset, (10_000 - 1) * 20);
    }

    /// Regression: a colossal `page` (e.g. `i64::MAX` from the query string)
    /// must not overflow `(page - 1) * per_page` — that panics under debug
    /// overflow checks and wraps to a garbage offset in release. `resolve` now
    /// saturates, so the offset pins to `i64::MAX` (a past-the-end empty page)
    /// instead.
    #[test]
    fn resolve_saturates_offset_on_overflowing_page() {
        let cfg = test_config();
        let p = params(Some(i64::MAX), Some(1000)).resolve(&cfg);
        assert_eq!(p.page, i64::MAX);
        assert_eq!(p.per_page, 1000);
        assert_eq!(
            p.offset,
            i64::MAX,
            "offset must saturate, not overflow/wrap"
        );
    }

    // ── PaginationResult metadata on empty / past-end totals ─────────────
    //
    // `PaginationResult::page()` must produce sane metadata when `total = 0`
    // or when `offset > total` (user asked for a page past the data).

    #[test]
    fn pagination_result_total_zero_yields_zero_pages_and_no_nav() {
        let docs: Vec<Document> = Vec::new();
        let pr = PaginationResult::builder(&docs, 0, 20).page(1, 0);
        assert_eq!(pr.total_docs, 0);
        assert_eq!(pr.total_pages, Some(0));
        assert!(!pr.has_next_page);
        assert!(!pr.has_prev_page);
        assert_eq!(pr.next_page, None);
        assert_eq!(pr.prev_page, None);
    }

    #[test]
    fn pagination_result_offset_past_total_has_no_next_page() {
        // 5 total, per_page 10, page 3, offset 20 → past the data. Returned
        // docs would be empty at the query layer; the result still reports
        // total_pages = 1 (ceil(5/10)) and no further pages.
        let docs: Vec<Document> = Vec::new();
        let pr = PaginationResult::builder(&docs, 5, 10).page(3, 20);
        assert_eq!(pr.total_docs, 5);
        assert_eq!(pr.total_pages, Some(1));
        assert!(
            !pr.has_next_page,
            "page past last shouldn't advertise a next page"
        );
        assert!(
            pr.has_prev_page,
            "page past last should still have a prev page"
        );
    }
}
