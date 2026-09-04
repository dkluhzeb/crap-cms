//! Page-size limits and offset/cursor pagination mode.

use serde::{Deserialize, Serialize};

/// Controls default and maximum page sizes, and pagination mode.
#[derive(Debug, Clone, Deserialize, Serialize, crap_cms_macros::ConfigKeys)]
#[serde(default, deny_unknown_fields)]
pub struct PaginationConfig {
    /// Default page size when request doesn't specify a limit.
    pub default_limit: i64,
    /// Maximum allowed limit. Requests above this are clamped.
    pub max_limit: i64,
    /// Pagination mode: `"page"` (offset-based, default) or `"cursor"` (keyset-based).
    pub mode: PaginationMode,
}

impl Default for PaginationConfig {
    fn default() -> Self {
        Self {
            default_limit: 20,
            max_limit: 1000,
            mode: PaginationMode::Page,
        }
    }
}

impl PaginationConfig {
    /// Whether cursor-based pagination is active.
    #[must_use]
    pub fn is_cursor(&self) -> bool {
        matches!(self.mode, PaginationMode::Cursor)
    }

    /// Clamp a requested page size to the configured bounds: an absent request
    /// falls back to `default_limit`, and the result is floored to at least 1
    /// and capped at `max_limit` (itself floored to 1 to stay a valid clamp
    /// range on a degenerate config). The single chokepoint for turning a raw
    /// limit into a safe SQL `LIMIT`, so no read surface can emit `LIMIT 0`
    /// (silently empty page) or an unbounded query.
    #[must_use]
    pub fn resolve_limit(&self, requested: Option<i64>) -> i64 {
        let max = self.max_limit.max(1);
        requested.unwrap_or(self.default_limit).clamp(1, max)
    }
}

/// Pagination strategy.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PaginationMode {
    /// Offset-based pagination (page numbers).
    Page,
    /// Keyset-based pagination (cursors).
    Cursor,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{from_value, json};

    #[test]
    fn default_is_offset_paging_with_sane_limits() {
        let c = PaginationConfig::default();
        assert_eq!(c.default_limit, 20);
        assert_eq!(c.max_limit, 1000);
        assert_eq!(c.mode, PaginationMode::Page);
        assert!(!c.is_cursor());
    }

    #[test]
    fn is_cursor_reflects_the_mode() {
        let c = PaginationConfig {
            mode: PaginationMode::Cursor,
            ..Default::default()
        };
        assert!(c.is_cursor());
    }

    #[test]
    fn mode_deserializes_from_lowercase_strings() {
        assert_eq!(
            from_value::<PaginationMode>(json!("cursor")).unwrap(),
            PaginationMode::Cursor
        );
        assert_eq!(
            from_value::<PaginationMode>(json!("page")).unwrap(),
            PaginationMode::Page
        );
        assert!(from_value::<PaginationMode>(json!("Cursor")).is_err());
    }

    #[test]
    fn resolve_limit_floors_zero_and_negative_to_one() {
        // Regression: the admin relationship-search handler reimplemented limit
        // resolution and omitted the floor-to-1, so `?limit=0` produced
        // `LIMIT 0` — a silently empty result set. `resolve_limit` is the
        // shared chokepoint that floors to 1, matching `PaginationParams::resolve`.
        let c = PaginationConfig::default();
        assert_eq!(c.resolve_limit(Some(0)), 1, "limit 0 must floor to 1");
        assert_eq!(
            c.resolve_limit(Some(-5)),
            1,
            "negative limit must floor to 1"
        );
    }

    #[test]
    fn resolve_limit_defaults_and_caps() {
        let c = PaginationConfig::default();
        assert_eq!(
            c.resolve_limit(None),
            20,
            "absent limit falls back to default"
        );
        assert_eq!(c.resolve_limit(Some(5)), 5, "in-range limit passes through");
        assert_eq!(
            c.resolve_limit(Some(50_000)),
            1000,
            "over-max limit caps at max_limit"
        );
    }

    #[test]
    fn resolve_limit_survives_degenerate_max_of_zero() {
        // A misconfigured `max_limit = 0` must not panic the `clamp` (min > max);
        // max floors to 1 so the range stays valid.
        let c = PaginationConfig {
            default_limit: 20,
            max_limit: 0,
            mode: PaginationMode::Page,
        };
        assert_eq!(c.resolve_limit(Some(10)), 1);
        assert_eq!(c.resolve_limit(None), 1);
    }

    #[test]
    fn empty_table_uses_all_defaults() {
        let c: PaginationConfig = from_value(json!({})).unwrap();
        assert_eq!(c.default_limit, 20);
        assert_eq!(c.mode, PaginationMode::Page);
    }
}
