//! Cache backend factory — creates the appropriate backend from config.

use std::sync::Arc;

use anyhow::{Result, bail};
use tracing::{info, warn};

use crate::config::{CacheBackend, CacheConfig, CrapConfig};
use crate::core::lua_lease::LuaVmLease;

use super::{CustomCache, MemoryCache, NoneCache, SharedCache};

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
                config.redis_url.as_str(),
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
        // The custom backend needs a Lua VM lease — it is constructed by
        // [`create_cache_with_lease`] once the hook runner exists. Reaching
        // this arm means a caller without a runner asked for it.
        CacheBackend::Custom => {
            bail!(
                "[cache] backend = \"custom\" requires the Lua runtime — \
                 constructed via create_cache_with_lease at startup"
            );
        }
    }
}

/// Create the cache backend, with a Lua VM lease available for the `custom`
/// backend. Non-custom backends delegate to [`create_cache`].
///
/// For `custom`, the registration is verified immediately: `[cache]
/// backend = "custom"` without a `crap.cache.register(...)` call in
/// `init.lua` fails the boot here with a clear message instead of erroring
/// on the first cached read.
///
/// # Errors
///
/// Returns an error if the backend fails to initialize, or (for `custom`)
/// when no handler is registered on the leased VM.
pub fn create_cache_with_lease(
    config: &CacheConfig,
    lease: Arc<dyn LuaVmLease>,
) -> Result<SharedCache> {
    if config.backend == CacheBackend::Custom {
        info!("Using custom (Lua-delegated) cache backend");

        let cache = CustomCache::new(lease);
        cache.verify_registered()?;

        return Ok(Arc::new(cache));
    }

    create_cache(config)
}

/// Log the custom cache's per-VM statelessness contract when the hook-VM
/// pool can exceed one VM (the normal case). `init.lua` runs once *per*
/// pool VM, so a handler holding state in a Lua table keeps one store per
/// VM — a read cached on one VM survives a write-through clear that ran on
/// another, serving stale documents indefinitely. This can't be detected
/// mechanically through the lease (there is no way to pick which VM serves
/// a call), so it is surfaced loudly at boot instead.
pub fn warn_if_custom_cache_multi_vm(config: &CrapConfig) {
    if config.cache.backend == CacheBackend::Custom && config.hooks.max_vm_pool_size > 1 {
        warn!(
            max_vms = config.hooks.max_vm_pool_size,
            "custom cache handlers run across up to {} pooled Lua VMs — the handler must \
             delegate to a shared external store (per-VM Lua state serves stale data after \
             write-through clears); see the crap.cache docs",
            config.hooks.max_vm_pool_size
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lua_lease::LocalLease;

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

    /// Regression: `backend = "custom"` used to hand back a silent no-op
    /// placeholder; it now requires the lease-based constructor, and that
    /// constructor fails fast when `crap.cache.register` was never called.
    #[test]
    fn create_custom_without_lease_errors() {
        let config = CacheConfig {
            backend: CacheBackend::Custom,
            ..Default::default()
        };
        let err = create_cache(&config)
            .err()
            .expect("custom without lease must fail")
            .to_string();
        assert!(err.contains("create_cache_with_lease"), "{err}");
    }

    #[test]
    fn create_custom_with_lease_verifies_registration() {
        let config = CacheConfig {
            backend: CacheBackend::Custom,
            ..Default::default()
        };

        // Unregistered VM → boot-time error naming the register call.
        let lua = mlua::Lua::new();
        lua.load("crap = {}").exec().unwrap();
        let err = create_cache_with_lease(&config, Arc::new(LocalLease::new(&lua)))
            .err()
            .expect("unregistered VM must fail")
            .to_string();
        assert!(err.contains("crap.cache.register"), "{err}");

        // Registered VM → working backend.
        lua.load(
            r"
            local store = {}
            crap._cache = {
                get = function(k) return store[k] end,
                set = function(k, v) store[k] = v end,
                delete = function(k) store[k] = nil end,
                clear = function() store = {} end,
            }
            ",
        )
        .exec()
        .unwrap();
        let cache = create_cache_with_lease(&config, Arc::new(LocalLease::new(&lua))).unwrap();
        assert_eq!(cache.kind(), "custom");
        cache.set("k", b"v").unwrap();
        assert_eq!(cache.get("k").unwrap().unwrap(), b"v");

        // Non-custom configs pass through to the plain factory.
        let mem = create_cache_with_lease(&CacheConfig::default(), Arc::new(LocalLease::new(&lua)))
            .unwrap();
        assert_eq!(mem.kind(), "memory");
    }
}
