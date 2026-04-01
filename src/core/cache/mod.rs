//! Cache backend abstraction for cross-request caching.
//!
//! Provides a trait-based backend system: `memory` (default DashMap),
//! `redis` (feature-flagged), `none` (no-op), and `custom` (Lua-delegated).

mod custom;
mod memory;
mod none;
#[cfg(feature = "redis")]
mod redis;

use std::sync::Arc;

use anyhow::Result;
use tracing::info;

pub use custom::CustomCache;
pub use memory::MemoryCache;
pub use none::NoneCache;

use crate::config::CacheConfig;

/// Thread-safe shared reference to a cache backend.
pub type SharedCache = Arc<dyn CacheBackend>;

/// Object-safe cache backend trait.
///
/// Keys are arbitrary strings. Values are opaque byte slices — callers handle
/// serialization. All methods are synchronous (called from `spawn_blocking`).
pub trait CacheBackend: Send + Sync {
    /// Retrieve a cached value. Returns `None` on cache miss.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;

    /// Store a value. Overwrites any existing entry for the key.
    fn set(&self, key: &str, value: &[u8]) -> Result<()>;

    /// Remove a single key. No error if the key doesn't exist.
    fn delete(&self, key: &str) -> Result<()>;

    /// Remove all entries from the cache.
    fn clear(&self) -> Result<()>;

    /// Check whether a key exists without retrieving its value.
    fn has(&self, key: &str) -> Result<bool>;

    /// Return the backend identifier (`"memory"`, `"redis"`, `"none"`, `"custom"`).
    fn kind(&self) -> &'static str;
}

/// No-op placeholder that reports `kind() = "custom"` for diagnostics.
/// Used when `backend = "custom"` is selected but Lua init hasn't run yet.
struct CustomPlaceholder;

impl CacheBackend for CustomPlaceholder {
    fn get(&self, _key: &str) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn set(&self, _key: &str, _value: &[u8]) -> Result<()> {
        Ok(())
    }
    fn delete(&self, _key: &str) -> Result<()> {
        Ok(())
    }
    fn clear(&self) -> Result<()> {
        Ok(())
    }
    fn has(&self, _key: &str) -> Result<bool> {
        Ok(false)
    }
    fn kind(&self) -> &'static str {
        "custom"
    }
}

/// Create the appropriate cache backend from config.
pub fn create_cache(config: &CacheConfig) -> Result<SharedCache> {
    match config.backend.as_str() {
        "memory" | "" => {
            info!(
                max_entries = config.max_entries,
                "Using memory cache backend"
            );
            Ok(Arc::new(MemoryCache::new(config.max_entries)))
        }
        "none" => {
            info!("Using no-op cache backend");
            Ok(Arc::new(NoneCache))
        }
        #[cfg(feature = "redis")]
        "redis" => {
            info!(url = %config.redis_url, prefix = %config.prefix, "Using Redis cache backend");
            Ok(Arc::new(redis::RedisCache::new(
                &config.redis_url,
                &config.prefix,
                config.max_age_secs,
            )?))
        }
        #[cfg(not(feature = "redis"))]
        "redis" => {
            anyhow::bail!(
                "Redis cache backend requires the `redis` feature. \
                 Rebuild with `--features redis`."
            );
        }
        "custom" => {
            info!("Custom cache backend selected — waiting for Lua init");
            Ok(Arc::new(CustomPlaceholder))
        }
        other => anyhow::bail!("Unknown cache backend: '{}'", other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_memory_cache_default() {
        let config = CacheConfig::default();
        let cache = create_cache(&config).unwrap();
        assert_eq!(cache.kind(), "memory");
    }

    #[test]
    fn create_none_cache() {
        let config = CacheConfig {
            backend: "none".to_string(),
            ..Default::default()
        };
        let cache = create_cache(&config).unwrap();
        assert_eq!(cache.kind(), "none");
    }

    #[test]
    fn create_unknown_backend_errors() {
        let config = CacheConfig {
            backend: "memcached".to_string(),
            ..Default::default()
        };
        let result = create_cache(&config);
        assert!(result.is_err());

        let err = result.err().unwrap();
        assert!(err.to_string().contains("memcached"));
    }

    #[test]
    fn create_custom_uses_placeholder() {
        let config = CacheConfig {
            backend: "custom".to_string(),
            ..Default::default()
        };
        let cache = create_cache(&config).unwrap();
        assert_eq!(cache.kind(), "custom");

        // Placeholder behaves as no-op
        cache.set("k", b"v").unwrap();
        assert!(cache.get("k").unwrap().is_none());
    }
}
