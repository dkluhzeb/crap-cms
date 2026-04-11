//! Shared paginated result type for all multi-result service functions.

use crate::db::query::PaginationResult;

/// Paginated result — returned by all multi-result service functions.
///
/// Contains the result items, total count, and computed pagination metadata.
/// Callers use the `pagination` field directly for response formatting.
pub struct PaginatedResult<T> {
    /// The result items for this page.
    pub docs: Vec<T>,
    /// Total count of matching items across all pages.
    pub total: i64,
    /// Computed pagination metadata (page info or cursor info).
    pub pagination: PaginationResult,
}

impl<T> Default for PaginatedResult<T> {
    fn default() -> Self {
        Self {
            docs: Vec::new(),
            total: 0,
            pagination: PaginationResult::builder(&[], 0, 0).page(1, 0),
        }
    }
}
