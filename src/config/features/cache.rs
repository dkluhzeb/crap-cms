//! Cross-request cache backend configuration.

use serde::{Deserialize, Serialize};

/// Cache backend configuration.
///
/// Configures the cross-request cache used for relationship population and
/// other cacheable data. The cache is cleared on any write operation.
///
/// ```toml
/// [cache]
/// backend = "memory"      # "memory" (default), "redis", "none", "custom"
/// max_entries = 10000      # soft cap for memory backend
/// max_age_secs = 0         # periodic full clear (0 = disabled)
/// redis_url = "redis://127.0.0.1:6379"
/// prefix = "crap:"         # key prefix for Redis
/// ```
/// Cross-request cache backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheBackend {
    /// In-process cache (the default).
    #[default]
    Memory,
    /// Redis-backed cache (requires the `redis` feature).
    Redis,
    /// Caching disabled.
    None,
    /// Lua-delegated backend registered via `crap.cache.register` in
    /// `init.lua`. Boot fails when selected without a registration.
    /// TTL/expiry and cluster-wide `clear` are the handler's concern;
    /// `max_entries`, `max_age_secs`, `redis_url` and `prefix` do not apply.
    Custom,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CacheConfig {
    /// Cache backend: `memory` (default), `redis`, `none`, or `custom`.
    pub backend: CacheBackend,
    /// Soft cap on the number of entries for the memory backend.
    /// Once reached, new insertions are skipped until a clear. Default: 10,000.
    #[serde(default = "default_cache_max_entries")]
    pub max_entries: usize,
    /// Periodic full clear interval in seconds. 0 = disabled (only
    /// write-through invalidation). Set > 0 to handle external DB mutations.
    #[serde(default)]
    pub max_age_secs: u64,
    /// Redis connection URL. Only used when `backend = "redis"`.
    #[serde(default = "default_redis_url")]
    pub redis_url: String,
    /// Key prefix for the Redis backend. All keys are stored as `{prefix}{key}`.
    #[serde(default = "default_cache_prefix")]
    pub prefix: String,
}

fn default_cache_max_entries() -> usize {
    10_000
}

fn default_redis_url() -> String {
    "redis://127.0.0.1:6379".to_string()
}

fn default_cache_prefix() -> String {
    "crap:".to_string()
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            backend: CacheBackend::default(),
            max_entries: default_cache_max_entries(),
            max_age_secs: 0,
            redis_url: default_redis_url(),
            prefix: default_cache_prefix(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_config_defaults() {
        let cache = CacheConfig::default();
        assert_eq!(cache.backend, CacheBackend::Memory);
        assert_eq!(cache.max_entries, 10_000);
        assert_eq!(cache.max_age_secs, 0);
        assert_eq!(cache.redis_url, "redis://127.0.0.1:6379");
        assert_eq!(cache.prefix, "crap:");
    }

    #[test]
    fn cache_config_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[cache]\nbackend = \"none\"\nmax_entries = 5000\nmax_age_secs = 60\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.cache.backend, CacheBackend::None);
        assert_eq!(config.cache.max_entries, 5000);
        assert_eq!(config.cache.max_age_secs, 60);
    }

    #[test]
    fn cache_config_partial_toml_uses_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[cache]\nbackend = \"redis\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.cache.backend, CacheBackend::Redis);
        assert_eq!(config.cache.max_entries, 10_000);
        assert_eq!(config.cache.redis_url, "redis://127.0.0.1:6379");
        assert_eq!(config.cache.prefix, "crap:");
    }

    #[test]
    fn cache_max_entries_zero_warns_but_passes() {
        let mut config = crate::config::CrapConfig::default();
        config.cache.max_entries = 0;
        // Should warn but not error
        assert!(config.validate().is_ok());
    }
}
