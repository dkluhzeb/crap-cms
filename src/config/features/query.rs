//! Caps on the size of a caller-supplied query: `where` width and `search`
//! length.
//!
//! Every read and bulk-write surface (admin, gRPC, MCP, Lua CRUD) hands its
//! query to the same service validation, which checks it against these
//! limits before any SQL is built — so one request cannot pin a database
//! connection with a thousands-of-terms `where` or a megabyte search term.
//! Operator-written access constraints are not user queries and are not
//! capped.

use std::{num::NonZeroUsize, sync::OnceLock};

use serde::{Deserialize, Serialize};

/// `[query]` — size limits applied to every user-supplied query.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, crap_cms_macros::ConfigKeys,
)]
#[serde(default, deny_unknown_fields)]
pub struct QueryConfig {
    /// Most filter conditions one `where` may hold, counted across every
    /// `or` group (`{ a = 1, or = { { b = 2 }, { c = 3, d = 4 } } }` is 4).
    pub max_filter_terms: NonZeroUsize,
    /// Most `in` / `not_in` list elements one `where` may hold, summed over
    /// every list in it.
    pub max_filter_values: NonZeroUsize,
    /// Longest `search` term, in characters.
    pub max_search_length: NonZeroUsize,
    /// Most whitespace-separated words one `search` term may hold.
    pub max_search_terms: NonZeroUsize,
}

/// The built-in limits — what a query is checked against until a config is
/// applied, and the `Default` of the section.
const DEFAULT_QUERY_LIMITS: QueryConfig = QueryConfig {
    max_filter_terms: NonZeroUsize::new(100).expect("non-zero"),
    max_filter_values: NonZeroUsize::new(1000).expect("non-zero"),
    max_search_length: NonZeroUsize::new(1000).expect("non-zero"),
    max_search_terms: NonZeroUsize::new(32).expect("non-zero"),
};

impl Default for QueryConfig {
    fn default() -> Self {
        DEFAULT_QUERY_LIMITS
    }
}

/// The process-wide limits, installed once when the config is applied.
static QUERY_LIMITS: OnceLock<QueryConfig> = OnceLock::new();

/// Install `limits` process-wide. The first install wins: the limits are fixed
/// for the process lifetime, like the data-nesting limit.
pub(crate) fn install_query_limits(limits: QueryConfig) {
    let _ = QUERY_LIMITS.set(limits);
}

/// The query limits in force: the applied config's, or the built-in defaults
/// before one is applied (CLI paths and unit tests that never install one).
#[must_use]
pub fn query_limits() -> &'static QueryConfig {
    QUERY_LIMITS.get().unwrap_or(&DEFAULT_QUERY_LIMITS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_bound_every_dimension() {
        let q = QueryConfig::default();

        assert_eq!(q.max_filter_terms.get(), 100);
        assert_eq!(q.max_filter_values.get(), 1000);
        assert_eq!(q.max_search_length.get(), 1000);
        assert_eq!(q.max_search_terms.get(), 32);
    }

    /// A zero limit would reject every query; it is refused when the config
    /// is parsed rather than discovered on the first request.
    #[test]
    fn a_zero_limit_is_refused_at_parse_time() {
        let err = toml::from_str::<QueryConfig>("max_filter_terms = 0").unwrap_err();

        assert!(err.to_string().contains("nonzero"), "{err}");
    }

    #[test]
    fn a_partial_section_keeps_the_other_defaults() {
        let q: QueryConfig = toml::from_str("max_search_terms = 8").expect("parses");

        assert_eq!(q.max_search_terms.get(), 8);
        assert_eq!(q.max_filter_terms.get(), 100);
    }
}
