//! Cache backend factory — creates the appropriate backend from config.

use std::sync::Arc;

use anyhow::{Result, bail};
use tracing::info;

use crate::config::CacheConfig;

use super::{CacheBackend, MemoryCache, NoneCache, SharedCache};

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

            Ok(Arc::new(super::redis::RedisCache::new(
                &config.redis_url,
                &config.prefix,
                config.max_age_secs,
            )?))
        }
        #[cfg(not(feature = "redis"))]
        "redis" => {
            bail!(
                "Redis cache backend requires the `redis` feature. \
                 Rebuild with `--features redis`."
            );
        }
        "custom" => {
            info!("Custom cache backend selected — waiting for Lua init");

            Ok(Arc::new(CustomPlaceholder))
        }
        other => bail!("Unknown cache backend: '{}'", other),
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
