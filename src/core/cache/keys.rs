//! Redis key layout for the cache.
//!
//! The cache lives in its own sub-namespace under the configured prefix, so a
//! cache clear — a wildcard `SCAN MATCH {prefix}cache:*` followed by `DEL` —
//! can only ever reach cache keys. It used to match `{prefix}*`, which on a
//! shared Redis also covered the rate limiter's `crap:rl:` keys: every content
//! write wiped every lockout in the cluster.
//!
//! Deliberately outside the `redis` feature gate, so the layout — and the
//! startup check that rejects a rate-limit prefix overlapping it — is always
//! compiled and tested.

/// Segment appended to the configured prefix for every cache key.
pub const CACHE_KEY_NAMESPACE: &str = "cache:";

/// The namespace every cache key lives under: `{prefix}cache:`.
#[must_use]
pub fn cache_namespace(prefix: &str) -> String {
    format!("{prefix}{CACHE_KEY_NAMESPACE}")
}

/// The full Redis key for one cache entry.
#[must_use]
pub fn cache_key(prefix: &str, key: &str) -> String {
    format!("{}{key}", cache_namespace(prefix))
}

/// The `SCAN MATCH` pattern a clear uses, confined to the cache namespace.
#[must_use]
pub fn cache_clear_pattern(prefix: &str) -> String {
    format!("{}*", cache_namespace(prefix))
}

#[cfg(test)]
mod tests {
    use super::{cache_clear_pattern, cache_key, cache_namespace};

    /// The founding bug: under the default prefixes the clear pattern matched
    /// every rate-limit key. A `prefix*` glob matches exactly the keys that
    /// start with `prefix`, so that is what this checks.
    #[test]
    fn a_cache_clear_cannot_reach_rate_limit_keys_under_default_prefixes() {
        let pattern = cache_clear_pattern("crap:");
        let glob = pattern
            .strip_suffix('*')
            .expect("pattern ends in a wildcard");

        assert!(!"crap:rl:login:victim@example.com".starts_with(glob));
        assert!(!"crap:rl:ip_login:203.0.113.7".starts_with(glob));
        assert!(
            cache_key("crap:", "populate:posts:1:en").starts_with(glob),
            "a clear must still reach cache keys"
        );
    }

    #[test]
    fn keys_live_under_the_cache_namespace() {
        assert_eq!(cache_namespace("crap:"), "crap:cache:");
        assert_eq!(
            cache_key("crap:", "populate:posts:1"),
            "crap:cache:populate:posts:1"
        );
        assert_eq!(cache_clear_pattern("crap:"), "crap:cache:*");
    }
}
