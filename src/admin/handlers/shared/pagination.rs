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
        let per_page = self
            .per_page
            .unwrap_or(config.default_limit)
            .clamp(1, config.max_limit);
        let offset = (page - 1) * per_page;

        Pagination {
            page,
            per_page,
            offset,
        }
    }
}
