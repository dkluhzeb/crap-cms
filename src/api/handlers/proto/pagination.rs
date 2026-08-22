//! Pagination conversion: `PaginationResult` -> gRPC `PaginationInfo`.

use crate::{api::content, db::query};

// `floor_optional_limit` (the optional-limit floor shared with the Lua/MCP
// version surfaces and the service layer) lives in `db::query`; re-exported here
// so gRPC callers keep the `proto::floor_optional_limit` path.
pub use crate::db::query::floor_optional_limit;

/// Convert a [`query::PaginationResult`] to a gRPC `PaginationInfo` message.
pub fn pagination_result_to_proto(pr: &query::PaginationResult) -> content::PaginationInfo {
    content::PaginationInfo {
        total_docs: pr.total_docs,
        limit: pr.limit,
        total_pages: pr.total_pages,
        page: pr.page,
        page_start: pr.page_start,
        has_prev_page: pr.has_prev_page,
        has_next_page: pr.has_next_page,
        prev_page: pr.prev_page,
        next_page: pr.next_page,
        start_cursor: pr.start_cursor.clone(),
        end_cursor: pr.end_cursor.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `None` (no explicit limit) is preserved; a negative `Some` is floored.
    #[test]
    fn floor_optional_limit_preserves_none_floors_negative() {
        assert_eq!(floor_optional_limit(None), None);
        assert_eq!(floor_optional_limit(Some(-1)), Some(0)); // the fail-open case
        assert_eq!(floor_optional_limit(Some(0)), Some(0));
        assert_eq!(floor_optional_limit(Some(25)), Some(25));
    }

    /// Every field gets a distinct value so a swapped assignment in the
    /// hand-written conversion (e.g. `page`/`page_start`, `prev`/`next`,
    /// `has_prev`/`has_next`, `start`/`end` cursor) surfaces as a mismatch.
    #[test]
    fn conversion_maps_each_field_to_its_own_slot() {
        let pr = query::PaginationResult {
            total_docs: 100,
            limit: 20,
            has_next_page: true,
            has_prev_page: false,
            total_pages: Some(5),
            page: Some(2),
            page_start: Some(21),
            prev_page: Some(1),
            next_page: Some(3),
            start_cursor: Some("start".into()),
            end_cursor: Some("end".into()),
        };

        let p = pagination_result_to_proto(&pr);

        assert_eq!(p.total_docs, 100);
        assert_eq!(p.limit, 20);
        assert!(p.has_next_page);
        assert!(!p.has_prev_page);
        assert_eq!(p.total_pages, Some(5));
        assert_eq!(p.page, Some(2));
        assert_eq!(p.page_start, Some(21));
        assert_eq!(p.prev_page, Some(1));
        assert_eq!(p.next_page, Some(3));
        assert_eq!(p.start_cursor.as_deref(), Some("start"));
        assert_eq!(p.end_cursor.as_deref(), Some("end"));
    }
}
