//! Unified pagination result struct and builder.
//!
//! Single source of truth for pagination output construction. All entry points
//! (gRPC, MCP, Admin, Lua) build a `PaginationResult` via the builder, then
//! convert to their format-specific representation with a thin adapter.

use serde::Serialize;

use crate::core::Document;

use super::cursor;

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
    pub fn cursor(
        self,
        order_by: Option<&str>,
        has_timestamps: bool,
        had_before_cursor: bool,
        had_any_cursor: bool,
    ) -> PaginationResult {
        let (sort_col, sort_dir) = resolve_sort(order_by, has_timestamps);
        let (start_cursor, end_cursor) = cursor::build_cursors(self.docs, &sort_col, &sort_dir);

        let at_limit = self.docs.len() as i64 >= self.limit && !self.docs.is_empty();
        let (has_next_page, has_prev_page) = if had_before_cursor {
            (true, at_limit)
        } else {
            (at_limit, had_any_cursor)
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
/// Returns `(column, direction)` where direction is `"ASC"` or `"DESC"`.
pub fn resolve_sort(order_by: Option<&str>, has_timestamps: bool) -> (String, String) {
    if let Some(order) = order_by {
        if let Some(stripped) = order.strip_prefix('-') {
            (stripped.to_string(), "DESC".to_string())
        } else {
            (order.to_string(), "ASC".to_string())
        }
    } else if has_timestamps {
        ("created_at".to_string(), "DESC".to_string())
    } else {
        ("id".to_string(), "ASC".to_string())
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
        assert_eq!(dir, "ASC");
    }

    #[test]
    fn resolve_sort_explicit_desc() {
        let (col, dir) = resolve_sort(Some("-created_at"), true);
        assert_eq!(col, "created_at");
        assert_eq!(dir, "DESC");
    }

    #[test]
    fn resolve_sort_default_with_timestamps() {
        let (col, dir) = resolve_sort(None, true);
        assert_eq!(col, "created_at");
        assert_eq!(dir, "DESC");
    }

    #[test]
    fn resolve_sort_default_without_timestamps() {
        let (col, dir) = resolve_sort(None, false);
        assert_eq!(col, "id");
        assert_eq!(dir, "ASC");
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
        let pr = PaginationResult::builder(&docs, 50, 10).cursor(Some("title"), true, false, false);
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
        let pr = PaginationResult::builder(&docs, 50, 10).cursor(Some("title"), true, false, true);
        assert!(pr.has_next_page);
        assert!(pr.has_prev_page);
    }

    #[test]
    fn cursor_backward() {
        let docs: Vec<Document> = (0..10).map(|i| make_doc(&format!("d{}", i))).collect();
        let pr = PaginationResult::builder(&docs, 50, 10).cursor(None, true, true, true);
        assert!(pr.has_next_page);
        assert!(pr.has_prev_page); // at_limit=true since 10 >= 10
    }

    #[test]
    fn cursor_backward_not_at_limit() {
        let docs: Vec<Document> = (0..3).map(|i| make_doc(&format!("d{}", i))).collect();
        let pr = PaginationResult::builder(&docs, 50, 10).cursor(None, true, true, true);
        assert!(pr.has_next_page);
        assert!(!pr.has_prev_page); // at_limit=false since 3 < 10
    }

    #[test]
    fn cursor_empty_results() {
        let docs: Vec<Document> = Vec::new();
        let pr = PaginationResult::builder(&docs, 0, 10).cursor(Some("title"), false, false, false);
        assert!(!pr.has_next_page);
        assert!(!pr.has_prev_page);
        assert_eq!(pr.start_cursor, None);
        assert_eq!(pr.end_cursor, None);
    }

    #[test]
    fn cursor_default_sort_with_timestamps() {
        let docs = vec![make_doc("a")];
        let pr = PaginationResult::builder(&docs, 1, 10).cursor(None, true, false, false);
        assert!(pr.start_cursor.is_some());
    }

    #[test]
    fn cursor_default_sort_without_timestamps() {
        let docs = vec![make_doc("a")];
        let pr = PaginationResult::builder(&docs, 1, 10).cursor(None, false, false, false);
        assert!(pr.start_cursor.is_some());
    }

    #[test]
    fn cursor_descending_sort() {
        let docs = vec![make_doc("a")];
        let pr =
            PaginationResult::builder(&docs, 1, 10).cursor(Some("-created_at"), true, false, false);
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
        let pr = PaginationResult::builder(&docs, 1, 10).cursor(None, false, false, false);
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
