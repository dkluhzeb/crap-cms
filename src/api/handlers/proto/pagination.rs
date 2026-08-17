//! Pagination conversion: `PaginationResult` -> gRPC `PaginationInfo`.

use crate::{api::content, db::query};

/// Clamp a client-supplied `limit` (with its default already applied) into
/// `[0, max]`. Without the floor a negative value binds as `LIMIT -1`, which
/// `SQLite` treats as *no limit* — a fail-open unbounded read that bypasses the
/// intended cap. The floor at 0 is fail-closed (`LIMIT 0` returns no rows).
pub fn clamp_limit(limit: i64, max: i64) -> i64 {
    limit.clamp(0, max)
}

/// Floor an optional `limit` at 0, preserving `None`. Used where `None` is an
/// intended, separately-bounded "no explicit limit" contract (e.g. version
/// history, capped by `max_versions`) — we must not turn `None` into a cap, but
/// a negative `Some(-1)` must not become an unbounded `LIMIT -1` read either.
pub fn floor_optional_limit(limit: Option<i64>) -> Option<i64> {
    limit.map(|l| l.max(0))
}

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

    /// A negative limit must not survive as `LIMIT -1` (unbounded on `SQLite`).
    #[test]
    fn clamp_limit_floors_negatives_and_caps_max() {
        assert_eq!(clamp_limit(-1, 1000), 0); // the fail-open case
        assert_eq!(clamp_limit(-9999, 1000), 0);
        assert_eq!(clamp_limit(0, 1000), 0);
        assert_eq!(clamp_limit(50, 1000), 50);
        assert_eq!(clamp_limit(5000, 1000), 1000); // capped
    }

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
