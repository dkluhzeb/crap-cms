//! Pagination conversion: `PaginationResult` -> gRPC `PaginationInfo`.

use crate::{api::content, db::query};

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
