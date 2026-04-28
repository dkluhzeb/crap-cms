//! Unified pagination result struct and builder.
//!
//! Single source of truth for pagination output construction. All entry points
//! (gRPC, MCP, Admin, Lua) build a `PaginationResult` via the builder, then
//! convert to their format-specific representation with a thin adapter.

use serde::Serialize;

use crate::core::Document;

use super::cursor::{self, SortDirection};

/// Unified pagination result — returned by the builder, consumed by entry-point converters.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaginationResult {
    pub total_docs: i64,
    pub limit: i64,
    pub has_next_page: bool,
    pub has_prev_page: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_pages: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_page: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_cursor: Option<String>,
}

impl PaginationResult {
    /// Start building a pagination result from query results.
    pub fn builder<'a>(
        docs: &'a [Document],
        total: i64,
        limit: i64,
    ) -> PaginationResultBuilder<'a> {
        PaginationResultBuilder::new(docs, total, limit)
    }
}

/// Builder with two terminal methods: `page()` and `cursor()`.
pub struct PaginationResultBuilder<'a> {
    docs: &'a [Document],
    total: i64,
    limit: i64,
}

impl<'a> PaginationResultBuilder<'a> {
    pub fn new(docs: &'a [Document], total: i64, limit: i64) -> Self {
        Self { docs, total, limit }
    }

    /// Terminal: compute page-based pagination result.
    pub fn page(self, page: i64, offset: i64) -> PaginationResult {
        let total_pages = if self.limit > 0 {
            (self.total + self.limit - 1) / self.limit
        } else {
            0
        };
        PaginationResult {
            total_docs: self.total,
            limit: self.limit,
            has_next_page: page < total_pages,
            has_prev_page: page > 1,
            total_pages: Some(total_pages),
            page: Some(page),
            page_start: Some(offset + 1),
            prev_page: if page > 1 { Some(page - 1) } else { None },
            next_page: if page < total_pages {
                Some(page + 1)
            } else {
                None
            },
            start_cursor: None,
            end_cursor: None,
        }
    }

    /// Terminal: compute cursor-based pagination result.
    ///
    /// When `cursor_has_more` is `Some(true/false)`, it's used as the authoritative
    /// signal for whether more docs exist in the current direction (from overfetch).
    /// When `None`, falls back to the `total`-based heuristic.
    ///
    /// `has_drafts` records whether the collection has drafts enabled and so the
    /// `find` SQL prepended `_status ASC` to the ORDER BY (see
    /// `apply_order_by`). When true and the user's sort isn't `_status` itself,
    /// each cursor also encodes the row's `_status` so prev/next stays
    /// symmetric across the draft↔published boundary.
    pub fn cursor(
        self,
        order_by: Option<&str>,
        has_timestamps: bool,
        has_drafts: bool,
        had_before_cursor: bool,
        had_any_cursor: bool,
        cursor_has_more: Option<bool>,
    ) -> PaginationResult {
        let (sort_col, sort_dir) = resolve_sort(order_by, has_timestamps);
        let with_status = cursor::cursor_status_active(has_drafts, &sort_col);
        let (start_cursor, end_cursor) =
            cursor::build_cursors(self.docs, &sort_col, sort_dir, with_status);

        let total_pages = if self.limit > 0 {
            (self.total + self.limit - 1) / self.limit
        } else {
            1
        };
        let multiple_pages = total_pages > 1;

        // cursor_has_more (from overfetch) is authoritative when available.
        // Fallback: at_limit heuristic (docs.len() >= limit implies more pages).
        let at_limit = self.docs.len() as i64 >= self.limit && !self.docs.is_empty();
        let more_in_direction = cursor_has_more.unwrap_or(at_limit);

        let (has_next_page, has_prev_page) = if had_before_cursor {
            // Navigating backwards: always has next (we came from there),
            // has prev only if overfetch/heuristic says more exist behind us.
            (true, more_in_direction)
        } else if had_any_cursor {
            // Navigating forwards with after_cursor: always has prev (we came from there),
            // has next if overfetch/heuristic says more exist ahead.
            (more_in_direction, true)
        } else {
            // Initial load (no cursor): no prev, has next if more than one page of total.
            (multiple_pages, false)
        };

        PaginationResult {
            total_docs: self.total,
            limit: self.limit,
            has_next_page,
            has_prev_page,
            total_pages: None,
            page: None,
            page_start: None,
            prev_page: None,
            next_page: None,
            start_cursor,
            end_cursor,
        }
    }
}

/// Resolve sort column and direction from an `order_by` string.
///
/// Returns `(column, direction)`. A leading `-` means descending.
pub fn resolve_sort(order_by: Option<&str>, has_timestamps: bool) -> (String, SortDirection) {
    match order_by {
        Some(order) if order.starts_with('-') => (order[1..].to_string(), SortDirection::Desc),
        Some(order) => (order.to_string(), SortDirection::Asc),
        None if has_timestamps => ("created_at".to_string(), SortDirection::Desc),
        None => ("id".to_string(), SortDirection::Asc),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::DocumentId;
    use std::collections::HashMap;

    fn make_doc(id: &str) -> Document {
        Document {
            id: DocumentId::new(id),
            fields: HashMap::new(),
            created_at: None,
            updated_at: None,
        }
    }

    // ── resolve_sort ──────────────────────────────────────────────────

    #[test]
    fn resolve_sort_explicit_asc() {
        let (col, dir) = resolve_sort(Some("title"), false);
        assert_eq!(col, "title");
        assert_eq!(dir, SortDirection::Asc);
    }

    #[test]
    fn resolve_sort_explicit_desc() {
        let (col, dir) = resolve_sort(Some("-created_at"), true);
        assert_eq!(col, "created_at");
        assert_eq!(dir, SortDirection::Desc);
    }

    #[test]
    fn resolve_sort_default_with_timestamps() {
        let (col, dir) = resolve_sort(None, true);
        assert_eq!(col, "created_at");
        assert_eq!(dir, SortDirection::Desc);
    }

    #[test]
    fn resolve_sort_default_without_timestamps() {
        let (col, dir) = resolve_sort(None, false);
        assert_eq!(col, "id");
        assert_eq!(dir, SortDirection::Asc);
    }

    // ── Page-based ────────────────────────────────────────────────────

    #[test]
    fn page_single_page() {
        let docs = vec![make_doc("a"), make_doc("b")];
        let pr = PaginationResult::builder(&docs, 2, 10).page(1, 0);
        assert_eq!(pr.total_docs, 2);
        assert_eq!(pr.limit, 10);
        assert_eq!(pr.total_pages, Some(1));
        assert_eq!(pr.page, Some(1));
        assert_eq!(pr.page_start, Some(1));
        assert!(!pr.has_prev_page);
        assert!(!pr.has_next_page);
        assert_eq!(pr.prev_page, None);
        assert_eq!(pr.next_page, None);
        assert_eq!(pr.start_cursor, None);
        assert_eq!(pr.end_cursor, None);
    }

    #[test]
    fn page_multi_page() {
        let docs = vec![make_doc("a"), make_doc("b")];
        let pr = PaginationResult::builder(&docs, 25, 10).page(2, 10);
        assert_eq!(pr.total_pages, Some(3));
        assert_eq!(pr.page, Some(2));
        assert_eq!(pr.page_start, Some(11));
        assert!(pr.has_prev_page);
        assert!(pr.has_next_page);
        assert_eq!(pr.prev_page, Some(1));
        assert_eq!(pr.next_page, Some(3));
    }

    #[test]
    fn page_last_page() {
        let docs = vec![make_doc("a")];
        let pr = PaginationResult::builder(&docs, 21, 10).page(3, 20);
        assert_eq!(pr.total_pages, Some(3));
        assert!(pr.has_prev_page);
        assert!(!pr.has_next_page);
        assert_eq!(pr.prev_page, Some(2));
        assert_eq!(pr.next_page, None);
    }

    #[test]
    fn page_empty_results() {
        let docs: Vec<Document> = Vec::new();
        let pr = PaginationResult::builder(&docs, 0, 10).page(1, 0);
        assert_eq!(pr.total_docs, 0);
        assert_eq!(pr.total_pages, Some(0));
        assert!(!pr.has_prev_page);
        assert!(!pr.has_next_page);
    }

    // ── Cursor-based ──────────────────────────────────────────────────

    #[test]
    fn cursor_forward_with_more() {
        let docs: Vec<Document> = (0..10).map(|i| make_doc(&format!("d{}", i))).collect();
        let pr = PaginationResult::builder(&docs, 50, 10).cursor(
            Some("title"),
            true,
            false,
            false,
            false,
            None,
        );
        assert!(pr.has_next_page);
        assert!(!pr.has_prev_page);
        assert!(pr.start_cursor.is_some());
        assert!(pr.end_cursor.is_some());
        assert_eq!(pr.total_pages, None);
        assert_eq!(pr.page, None);
    }

    #[test]
    fn cursor_forward_after_cursor() {
        let docs: Vec<Document> = (0..10).map(|i| make_doc(&format!("d{}", i))).collect();
        let pr = PaginationResult::builder(&docs, 50, 10).cursor(
            Some("title"),
            true,
            false,
            false,
            true,
            None,
        );
        assert!(pr.has_next_page);
        assert!(pr.has_prev_page);
    }

    #[test]
    fn cursor_backward() {
        let docs: Vec<Document> = (0..10).map(|i| make_doc(&format!("d{}", i))).collect();
        let pr =
            PaginationResult::builder(&docs, 50, 10).cursor(None, true, false, true, true, None);
        assert!(pr.has_next_page);
        assert!(pr.has_prev_page); // at_limit=true since 10 >= 10
    }

    #[test]
    fn cursor_backward_not_at_limit() {
        let docs: Vec<Document> = (0..3).map(|i| make_doc(&format!("d{}", i))).collect();
        let pr =
            PaginationResult::builder(&docs, 50, 10).cursor(None, true, false, true, true, None);
        assert!(pr.has_next_page);
        assert!(!pr.has_prev_page); // at_limit=false since 3 < 10
    }

    #[test]
    fn cursor_initial_load_single_page() {
        // Exactly one page of results — next should be false
        let docs: Vec<Document> = (0..10).map(|i| make_doc(&format!("d{}", i))).collect();
        let pr =
            PaginationResult::builder(&docs, 10, 10).cursor(None, true, false, false, false, None);
        assert!(!pr.has_next_page, "Single page should not have next");
        assert!(!pr.has_prev_page, "Initial load should not have prev");
    }

    #[test]
    fn cursor_back_to_first_page_no_prev() {
        // Navigated back to first page via before_cursor with overfetch — prev should be false
        // when overfetch signals no more pages behind
        let docs: Vec<Document> = (0..10).map(|i| make_doc(&format!("d{}", i))).collect();
        let pr = PaginationResult::builder(&docs, 10, 10).cursor(
            None,
            true,
            false,
            true,
            true,
            Some(false),
        );
        assert!(pr.has_next_page, "Should have next (came from there)");
        assert!(
            !pr.has_prev_page,
            "Should not have prev when overfetch says no more"
        );
    }

    #[test]
    fn cursor_empty_results() {
        let docs: Vec<Document> = Vec::new();
        let pr = PaginationResult::builder(&docs, 0, 10).cursor(
            Some("title"),
            false,
            false,
            false,
            false,
            None,
        );
        assert!(!pr.has_next_page);
        assert!(!pr.has_prev_page);
        assert_eq!(pr.start_cursor, None);
        assert_eq!(pr.end_cursor, None);
    }

    #[test]
    fn cursor_default_sort_with_timestamps() {
        let docs = vec![make_doc("a")];
        let pr =
            PaginationResult::builder(&docs, 1, 10).cursor(None, true, false, false, false, None);
        assert!(pr.start_cursor.is_some());
    }

    #[test]
    fn cursor_default_sort_without_timestamps() {
        let docs = vec![make_doc("a")];
        let pr =
            PaginationResult::builder(&docs, 1, 10).cursor(None, false, false, false, false, None);
        assert!(pr.start_cursor.is_some());
    }

    #[test]
    fn cursor_descending_sort() {
        let docs = vec![make_doc("a")];
        let pr = PaginationResult::builder(&docs, 1, 10).cursor(
            Some("-created_at"),
            true,
            false,
            false,
            false,
            None,
        );
        assert!(pr.start_cursor.is_some());
    }

    // ── Serialization ─────────────────────────────────────────────────

    #[test]
    fn serialize_page_mode_camel_case_omits_cursor_fields() {
        let docs = vec![make_doc("a")];
        let pr = PaginationResult::builder(&docs, 5, 10).page(1, 0);
        let json = serde_json::to_value(&pr).unwrap();

        // camelCase keys present
        assert!(json.get("totalDocs").is_some());
        assert!(json.get("hasNextPage").is_some());
        assert!(json.get("totalPages").is_some());
        assert!(json.get("pageStart").is_some());

        // cursor fields omitted (None → skip)
        assert!(json.get("startCursor").is_none());
        assert!(json.get("endCursor").is_none());
    }

    #[test]
    fn serialize_cursor_mode_omits_page_fields() {
        let docs = vec![make_doc("a")];
        let pr =
            PaginationResult::builder(&docs, 1, 10).cursor(None, false, false, false, false, None);
        let json = serde_json::to_value(&pr).unwrap();

        // cursor fields present
        assert!(json.get("startCursor").is_some());

        // page fields omitted (None → skip)
        assert!(json.get("totalPages").is_none());
        assert!(json.get("page").is_none());
        assert!(json.get("pageStart").is_none());
        assert!(json.get("prevPage").is_none());
        assert!(json.get("nextPage").is_none());
    }
}
