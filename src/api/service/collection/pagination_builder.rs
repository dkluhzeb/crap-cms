//! Builder for constructing pagination info proto messages.

use crate::{api::content, core::Document, db::query::cursor};

/// Builder for constructing `PaginationInfo` proto messages. Handles both
/// page-based and cursor-based pagination modes.
pub(super) struct PaginationBuilder<'a> {
    cursor_enabled: bool,
    documents: &'a [Document],
    total: i64,
    limit: i64,
    page: i64,
    offset: i64,
    order_by: Option<&'a str>,
    has_timestamps: bool,
    had_before_cursor: bool,
    had_any_cursor: bool,
}

impl<'a> PaginationBuilder<'a> {
    /// Start with required core params.
    pub fn new(documents: &'a [Document], total: i64, limit: i64) -> Self {
        Self {
            cursor_enabled: false,
            documents,
            total,
            limit,
            page: 1,
            offset: 0,
            order_by: None,
            has_timestamps: false,
            had_before_cursor: false,
            had_any_cursor: false,
        }
    }

    /// Set page-based pagination parameters.
    pub fn page(mut self, page: i64, offset: i64) -> Self {
        self.page = page;
        self.offset = offset;
        self
    }

    /// Enable cursor-based pagination mode with sort configuration.
    pub fn cursor_mode(mut self, order_by: Option<&'a str>, has_timestamps: bool) -> Self {
        self.cursor_enabled = true;
        self.order_by = order_by;
        self.has_timestamps = has_timestamps;
        self
    }

    /// Set cursor state flags for has_next/has_prev computation.
    pub fn cursor_state(mut self, had_before: bool, had_any: bool) -> Self {
        self.had_before_cursor = had_before;
        self.had_any_cursor = had_any;
        self
    }

    /// Build the `PaginationInfo` proto message.
    pub fn build(self) -> content::PaginationInfo {
        if self.cursor_enabled {
            let (sort_col, sort_dir) = if let Some(order) = self.order_by {
                if let Some(stripped) = order.strip_prefix('-') {
                    (stripped.to_string(), "DESC")
                } else {
                    (order.to_string(), "ASC")
                }
            } else if self.has_timestamps {
                ("created_at".to_string(), "DESC")
            } else {
                ("id".to_string(), "ASC")
            };

            let (start_cursor, end_cursor) =
                cursor::build_cursors(self.documents, &sort_col, sort_dir);

            let using_before = self.had_before_cursor;
            let at_limit = self.documents.len() as i64 >= self.limit && !self.documents.is_empty();
            let (has_next_page, has_prev_page) = if using_before {
                (true, at_limit)
            } else {
                (at_limit, self.had_any_cursor)
            };

            content::PaginationInfo {
                total_docs: self.total,
                limit: self.limit,
                total_pages: None,
                page: None,
                page_start: None,
                has_prev_page,
                has_next_page,
                prev_page: None,
                next_page: None,
                start_cursor,
                end_cursor,
            }
        } else {
            let total_pages = if self.limit > 0 {
                (self.total + self.limit - 1) / self.limit
            } else {
                0
            };
            content::PaginationInfo {
                total_docs: self.total,
                limit: self.limit,
                total_pages: Some(total_pages),
                page: Some(self.page),
                page_start: Some(self.offset + 1),
                has_prev_page: self.page > 1,
                has_next_page: self.page < total_pages,
                prev_page: if self.page > 1 {
                    Some(self.page - 1)
                } else {
                    None
                },
                next_page: if self.page < total_pages {
                    Some(self.page + 1)
                } else {
                    None
                },
                start_cursor: None,
                end_cursor: None,
            }
        }
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

    #[test]
    fn pagination_page_based_single_page() {
        let docs = vec![make_doc("a"), make_doc("b")];
        let p = PaginationBuilder::new(&docs, 2, 10).page(1, 0).build();
        assert_eq!(p.total_docs, 2);
        assert_eq!(p.limit, 10);
        assert_eq!(p.total_pages, Some(1));
        assert_eq!(p.page, Some(1));
        assert_eq!(p.page_start, Some(1));
        assert!(!p.has_prev_page);
        assert!(!p.has_next_page);
        assert_eq!(p.prev_page, None);
        assert_eq!(p.next_page, None);
        assert_eq!(p.start_cursor, None);
        assert_eq!(p.end_cursor, None);
    }

    #[test]
    fn pagination_page_based_multi_page() {
        let docs = vec![make_doc("a"), make_doc("b")];
        let p = PaginationBuilder::new(&docs, 25, 10).page(2, 10).build();
        assert_eq!(p.total_pages, Some(3));
        assert_eq!(p.page, Some(2));
        assert_eq!(p.page_start, Some(11));
        assert!(p.has_prev_page);
        assert!(p.has_next_page);
        assert_eq!(p.prev_page, Some(1));
        assert_eq!(p.next_page, Some(3));
    }

    #[test]
    fn pagination_page_based_last_page() {
        let docs = vec![make_doc("a")];
        let p = PaginationBuilder::new(&docs, 21, 10).page(3, 20).build();
        assert_eq!(p.total_pages, Some(3));
        assert!(p.has_prev_page);
        assert!(!p.has_next_page);
        assert_eq!(p.prev_page, Some(2));
        assert_eq!(p.next_page, None);
    }

    #[test]
    fn pagination_page_based_empty_results() {
        let docs: Vec<Document> = Vec::new();
        let p = PaginationBuilder::new(&docs, 0, 10).page(1, 0).build();
        assert_eq!(p.total_docs, 0);
        assert_eq!(p.total_pages, Some(0));
        assert!(!p.has_prev_page);
        assert!(!p.has_next_page);
    }

    #[test]
    fn pagination_cursor_forward_with_more() {
        // 10 docs returned with limit=10 → has_next=true, has_prev=false (no prior cursor)
        let docs: Vec<Document> = (0..10).map(|i| make_doc(&format!("d{}", i))).collect();
        let p = PaginationBuilder::new(&docs, 50, 10)
            .cursor_mode(Some("title"), true)
            .cursor_state(false, false)
            .build();
        assert!(p.has_next_page);
        assert!(!p.has_prev_page);
        assert!(p.start_cursor.is_some());
        assert!(p.end_cursor.is_some());
        assert_eq!(p.total_pages, None);
        assert_eq!(p.page, None);
    }

    #[test]
    fn pagination_cursor_forward_after_cursor() {
        // After cursor set → has_prev=true (we came from somewhere)
        let docs: Vec<Document> = (0..10).map(|i| make_doc(&format!("d{}", i))).collect();
        let p = PaginationBuilder::new(&docs, 50, 10)
            .cursor_mode(Some("title"), true)
            .cursor_state(false, true)
            .build();
        assert!(p.has_next_page);
        assert!(p.has_prev_page);
    }

    #[test]
    fn pagination_cursor_backward() {
        // Before cursor set → has_next=true (we came from ahead), has_prev depends on results
        let docs: Vec<Document> = (0..10).map(|i| make_doc(&format!("d{}", i))).collect();
        let p = PaginationBuilder::new(&docs, 50, 10)
            .cursor_mode(None, true)
            .cursor_state(true, true)
            .build();
        assert!(p.has_next_page);
        assert!(p.has_prev_page); // at_limit=true since 10 >= 10
    }

    #[test]
    fn pagination_cursor_backward_not_at_limit() {
        // Before cursor, fewer than limit results → reached the beginning
        let docs: Vec<Document> = (0..3).map(|i| make_doc(&format!("d{}", i))).collect();
        let p = PaginationBuilder::new(&docs, 50, 10)
            .cursor_mode(None, true)
            .cursor_state(true, true)
            .build();
        assert!(p.has_next_page);
        assert!(!p.has_prev_page); // at_limit=false since 3 < 10
    }

    #[test]
    fn pagination_cursor_empty_results() {
        let docs: Vec<Document> = Vec::new();
        let p = PaginationBuilder::new(&docs, 0, 10)
            .cursor_mode(Some("title"), false)
            .cursor_state(false, false)
            .build();
        assert!(!p.has_next_page);
        assert!(!p.has_prev_page);
        assert_eq!(p.start_cursor, None);
        assert_eq!(p.end_cursor, None);
    }

    #[test]
    fn pagination_cursor_default_sort_with_timestamps() {
        let docs = vec![make_doc("a")];
        let p = PaginationBuilder::new(&docs, 1, 10)
            .cursor_mode(None, true)
            .build();
        // Default sort with timestamps → created_at DESC
        assert!(p.start_cursor.is_some());
    }

    #[test]
    fn pagination_cursor_default_sort_without_timestamps() {
        let docs = vec![make_doc("a")];
        let p = PaginationBuilder::new(&docs, 1, 10)
            .cursor_mode(None, false)
            .build();
        // Default sort without timestamps → id ASC
        assert!(p.start_cursor.is_some());
    }

    #[test]
    fn pagination_cursor_descending_sort() {
        let docs = vec![make_doc("a")];
        let p = PaginationBuilder::new(&docs, 1, 10)
            .cursor_mode(Some("-created_at"), true)
            .build();
        assert!(p.start_cursor.is_some());
    }
}
