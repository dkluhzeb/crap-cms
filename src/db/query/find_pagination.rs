//! Pagination parameter validation for Find queries.
//!
//! Shared across all entry points (gRPC, MCP, Admin, Lua) for consistent
//! limit clamping, page/cursor conflict detection, and cursor decoding.

use super::cursor::CursorData;

/// Static pagination configuration — construct once, reuse across requests.
///
/// Groups the three config values (`default_limit`, `max_limit`, `cursor_enabled`)
/// that are constant for the lifetime of a service instance.
pub struct PaginationCtx {
    pub default_limit: i64,
    pub max_limit: i64,
    pub cursor_enabled: bool,
}

impl PaginationCtx {
    pub fn new(default_limit: i64, max_limit: i64, cursor_enabled: bool) -> Self {
        Self {
            default_limit,
            max_limit,
            cursor_enabled,
        }
    }

    /// Validate per-request pagination parameters against this context.
    pub fn validate(
        &self,
        req_limit: Option<i64>,
        req_page: Option<i64>,
        req_after_cursor: Option<&str>,
        req_before_cursor: Option<&str>,
    ) -> Result<FindPagination, String> {
        validate_find_pagination(
            req_limit,
            req_page,
            req_after_cursor,
            req_before_cursor,
            self.default_limit,
            self.max_limit,
            self.cursor_enabled,
        )
    }
}

/// Validated pagination parameters for a Find query.
#[derive(Debug)]
pub struct FindPagination {
    /// Clamped limit (always >= 1, <= max_limit).
    pub limit: i64,
    /// Byte offset for page-based pagination (0 when using cursors).
    pub offset: i64,
    /// Resolved page number (>= 1).
    pub page: i64,
    /// Forward cursor (mutually exclusive with before_cursor and page).
    pub after_cursor: Option<CursorData>,
    /// Backward cursor (mutually exclusive with after_cursor and page).
    pub before_cursor: Option<CursorData>,
}

impl FindPagination {
    /// Returns true if either cursor is set.
    pub fn has_cursor(&self) -> bool {
        self.after_cursor.is_some() || self.before_cursor.is_some()
    }
}

/// Validate and normalize pagination parameters from a Find request.
///
/// Returns `Err(String)` for invalid combinations (e.g., cursor + page,
/// both after and before cursors). Callers wrap in their own error type.
pub fn validate_find_pagination(
    req_limit: Option<i64>,
    req_page: Option<i64>,
    req_after_cursor: Option<&str>,
    req_before_cursor: Option<&str>,
    default_limit: i64,
    max_limit: i64,
    cursor_enabled: bool,
) -> Result<FindPagination, String> {
    let limit = super::apply_pagination_limits(req_limit, default_limit, max_limit);

    let page = req_page.unwrap_or(1).max(1);
    let offset = (page - 1).saturating_mul(limit);

    let (after_cursor, before_cursor) = if cursor_enabled {
        // page=1 is the default and should not conflict with cursors.
        // Only reject page > 1 combined with a cursor (intentional pagination conflict).
        let has_explicit_page = req_page.is_some_and(|p| p > 1);

        let ac = if let Some(s) = req_after_cursor {
            if has_explicit_page {
                return Err(
                    "Cannot use both after_cursor and page — they are mutually exclusive"
                        .to_string(),
                );
            }
            Some(CursorData::decode(s).map_err(|e| format!("Invalid cursor: {}", e))?)
        } else {
            None
        };
        let bc = if let Some(s) = req_before_cursor {
            if has_explicit_page {
                return Err(
                    "Cannot use both before_cursor and page — they are mutually exclusive"
                        .to_string(),
                );
            }
            if ac.is_some() {
                return Err(
                    "Cannot use both after_cursor and before_cursor — they are mutually exclusive"
                        .to_string(),
                );
            }
            Some(CursorData::decode(s).map_err(|e| format!("Invalid cursor: {}", e))?)
        } else {
            None
        };
        (ac, bc)
    } else {
        (None, None)
    };

    Ok(FindPagination {
        limit,
        offset,
        page,
        after_cursor,
        before_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_page_limit_defaults() {
        let p = validate_find_pagination(None, None, None, None, 20, 100, false).unwrap();
        assert_eq!(p.limit, 20);
        assert_eq!(p.page, 1);
        assert_eq!(p.offset, 0);
        assert!(p.after_cursor.is_none());
        assert!(p.before_cursor.is_none());
        assert!(!p.has_cursor());
    }

    #[test]
    fn explicit_page_and_limit() {
        let p = validate_find_pagination(Some(10), Some(3), None, None, 20, 100, false).unwrap();
        assert_eq!(p.limit, 10);
        assert_eq!(p.page, 3);
        assert_eq!(p.offset, 20); // (3-1)*10
    }

    #[test]
    fn page_zero_normalizes_to_one() {
        let p = validate_find_pagination(None, Some(0), None, None, 20, 100, false).unwrap();
        assert_eq!(p.page, 1);
        assert_eq!(p.offset, 0);
    }

    #[test]
    fn negative_page_normalizes_to_one() {
        let p = validate_find_pagination(None, Some(-5), None, None, 20, 100, false).unwrap();
        assert_eq!(p.page, 1);
        assert_eq!(p.offset, 0);
    }

    #[test]
    fn limit_clamped_to_max() {
        let p = validate_find_pagination(Some(500), None, None, None, 20, 100, false).unwrap();
        assert_eq!(p.limit, 100);
    }

    #[test]
    fn limit_minimum_one() {
        let p = validate_find_pagination(Some(0), None, None, None, 20, 100, false).unwrap();
        assert_eq!(p.limit, 1);
    }

    #[test]
    fn cursor_after_page_conflict() {
        let cursor = make_test_cursor();
        let err = validate_find_pagination(None, Some(2), Some(&cursor), None, 20, 100, true)
            .unwrap_err();
        assert!(err.contains("after_cursor and page"));
    }

    #[test]
    fn cursor_before_page_conflict() {
        let cursor = make_test_cursor();
        let err = validate_find_pagination(None, Some(2), None, Some(&cursor), 20, 100, true)
            .unwrap_err();
        assert!(err.contains("before_cursor and page"));
    }

    #[test]
    fn cursor_with_page_one_is_allowed() {
        let cursor = make_test_cursor();
        // page=1 is the default — should not conflict with cursors
        let p =
            validate_find_pagination(None, Some(1), Some(&cursor), None, 20, 100, true).unwrap();
        assert!(p.after_cursor.is_some());
    }

    #[test]
    fn cursor_both_after_and_before_conflict() {
        let cursor = make_test_cursor();
        let err = validate_find_pagination(None, None, Some(&cursor), Some(&cursor), 20, 100, true)
            .unwrap_err();
        assert!(err.contains("after_cursor and before_cursor"));
    }

    #[test]
    fn cursor_invalid_decode() {
        let err = validate_find_pagination(
            None,
            None,
            Some("not-valid-base64-json"),
            None,
            20,
            100,
            true,
        )
        .unwrap_err();
        assert!(err.contains("Invalid cursor"));
    }

    #[test]
    fn cursor_ignored_when_disabled() {
        // Even with cursor strings, they should be ignored when cursor_enabled=false
        let p =
            validate_find_pagination(None, None, Some("anything"), None, 20, 100, false).unwrap();
        assert!(p.after_cursor.is_none());
        assert!(p.before_cursor.is_none());
        assert!(!p.has_cursor());
    }

    #[test]
    fn valid_after_cursor() {
        let cursor = make_test_cursor();
        let p = validate_find_pagination(None, None, Some(&cursor), None, 20, 100, true).unwrap();
        assert!(p.after_cursor.is_some());
        assert!(p.before_cursor.is_none());
        assert!(p.has_cursor());
    }

    #[test]
    fn valid_before_cursor() {
        let cursor = make_test_cursor();
        let p = validate_find_pagination(None, None, None, Some(&cursor), 20, 100, true).unwrap();
        assert!(p.before_cursor.is_some());
        assert!(p.after_cursor.is_none());
        assert!(p.has_cursor());
    }

    // ── PaginationCtx ─────────────────────────────────────────────

    #[test]
    fn pagination_ctx_delegates_to_validate() {
        let ctx = PaginationCtx::new(20, 100, false);
        let p = ctx.validate(Some(10), Some(3), None, None).unwrap();
        assert_eq!(p.limit, 10);
        assert_eq!(p.page, 3);
        assert_eq!(p.offset, 20);
    }

    #[test]
    fn pagination_ctx_rejects_cursor_page_conflict() {
        let cursor = make_test_cursor();
        let ctx = PaginationCtx::new(20, 100, true);
        let err = ctx
            .validate(None, Some(2), Some(&cursor), None)
            .unwrap_err();
        assert!(err.contains("after_cursor and page"));
    }

    #[test]
    fn extreme_page_does_not_overflow() {
        let p = validate_find_pagination(Some(100), Some(i64::MAX), None, None, 20, 100, false)
            .unwrap();
        assert_eq!(p.limit, 100);
        assert_eq!(p.page, i64::MAX);
        // Must not panic — saturating_mul clamps to i64::MAX
        assert!(p.offset > 0);
    }

    /// Create a valid encoded cursor for testing.
    fn make_test_cursor() -> String {
        let data = CursorData {
            sort_col: "created_at".to_string(),
            sort_dir: "ASC".to_string(),
            sort_val: serde_json::Value::String("2024-01-01".to_string()),
            id: "doc-123".to_string(),
        };
        data.encode().unwrap()
    }
}
