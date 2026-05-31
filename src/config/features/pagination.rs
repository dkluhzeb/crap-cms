//! Page-size limits and offset/cursor pagination mode.

use serde::{Deserialize, Serialize};

/// Controls default and maximum page sizes, and pagination mode.
#[derive(Debug, Clone, Deserialize, Serialize)]
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
    fn empty_table_uses_all_defaults() {
        let c: PaginationConfig = from_value(json!({})).unwrap();
        assert_eq!(c.default_limit, 20);
        assert_eq!(c.mode, PaginationMode::Page);
    }
}
