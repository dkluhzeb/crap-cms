//! Cache backend factory — creates the appropriate backend from config.

use std::sync::Arc;

use anyhow::Result;
#[cfg(not(feature = "redis"))]
use anyhow::bail;
use tracing::info;

use crate::config::{CacheBackend, CacheConfig};

// Aliased to disambiguate from the `CacheBackend` config enum imported above.
use super::{CacheBackend as CacheBackendTrait, MemoryCache, NoneCache, SharedCache};

/// No-op placeholder that reports `kind() = "custom"` for diagnostics.
/// Used when `backend = "custom"` is selected but Lua init hasn't run yet.
struct CustomPlaceholder;

impl CacheBackendTrait for CustomPlaceholder {
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
///
/// # Errors
///
/// Returns an error if the backend name is unknown or the Redis client
/// fails to initialize.
pub fn create_cache(config: &CacheConfig) -> Result<SharedCache> {
    match config.backend {
        CacheBackend::Memory => {
            info!(
                max_entries = config.max_entries,
                "Using memory cache backend"
            );

            Ok(Arc::new(MemoryCache::new(config.max_entries)))
        }
        CacheBackend::None => {
            info!("Using no-op cache backend");

            Ok(Arc::new(NoneCache))
        }
        #[cfg(feature = "redis")]
        CacheBackend::Redis => {
            info!(url = %config.redis_url, prefix = %config.prefix, "Using Redis cache backend");

            Ok(Arc::new(super::redis::RedisCache::new(
                &config.redis_url,
                &config.prefix,
                config.max_age_secs,
            )?))
        }
        #[cfg(not(feature = "redis"))]
        CacheBackend::Redis => {
            bail!(
                "Redis cache backend requires the `redis` feature. \
                 Rebuild with `--features redis`."
            );
        }
        CacheBackend::Custom => {
            info!("Custom cache backend selected — waiting for Lua init");

            Ok(Arc::new(CustomPlaceholder))
        }
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
            backend: CacheBackend::None,
            ..Default::default()
        };
        let cache = create_cache(&config).unwrap();
        assert_eq!(cache.kind(), "none");
    }

    #[test]
    fn create_custom_uses_placeholder() {
        let config = CacheConfig {
            backend: CacheBackend::Custom,
            ..Default::default()
        };
        let cache = create_cache(&config).unwrap();
        assert_eq!(cache.kind(), "custom");

        // Placeholder behaves as no-op
        cache.set("k", b"v").unwrap();
        assert!(cache.get("k").unwrap().is_none());
    }
}
