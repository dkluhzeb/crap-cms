//! Custom Lua-delegated cache backend.
//!
//! Delegates all cache operations to user-provided Lua functions registered
//! via `crap.cache.register({ get, set, delete, clear, has })` in `init.lua`.
//! The VM that runs the functions is supplied by a [`LuaVmLease`] — the hook
//! runner's pooled lease for external callers (the populate cache on the
//! gRPC/admin/MCP read surfaces), or a `LocalLease` over the current VM when
//! used from inside a pool VM (write-through `clear_cache` from hooks and job
//! handlers), mirroring the custom upload-storage backend.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use mlua::{Function, Lua, Table, Value};

use crate::core::cache::CacheBackend;
use crate::core::lua_lease::LuaVmLease;

/// Custom cache backend that delegates to Lua functions.
pub struct CustomCache {
    lease: Arc<dyn LuaVmLease>,
}

impl CustomCache {
    /// Create a new custom cache backend backed by `lease`. The leased VM
    /// must have `crap._cache` registered (via `init.lua`).
    #[must_use]
    pub fn new(lease: Arc<dyn LuaVmLease>) -> Self {
        Self { lease }
    }

    /// Verify that `crap.cache.register` was called on the leased VM. Run
    /// once at startup so a missing registration fails the boot with a clear
    /// message instead of erroring on the first cached read.
    ///
    /// # Errors
    ///
    /// Returns an error when no `crap._cache` handler table is registered or
    /// the lease cannot supply a VM.
    pub fn verify_registered(&self) -> Result<()> {
        self.lease.with_vm(&mut |lua| {
            cache_table(lua)?;
            Ok(())
        })
    }
}

/// Look up the registered `crap._cache` handler table on a VM.
fn cache_table(lua: &Lua) -> Result<Table> {
    let crap: Table = lua
        .globals()
        .get("crap")
        .map_err(|e| anyhow!("crap global not found: {e}"))?;

    crap.get("_cache").map_err(|_| {
        anyhow!(
            "[cache] backend = \"custom\" but no handler is registered — \
             call crap.cache.register({{ get, set, delete, clear }}) in init.lua"
        )
    })
}

/// Look up a registered `crap._cache.<name>` function on a VM.
fn cache_fn(lua: &Lua, name: &str) -> Result<Function> {
    cache_table(lua)?
        .get(name)
        .map_err(|e| anyhow!("crap._cache.{name} not found: {e}"))
}

impl CacheBackend for CustomCache {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        // Contract: the handler returns the value (a binary-safe string) on
        // hit, `nil` on miss, and *raises* only on a real failure — which
        // propagates like a Redis error would.
        let mut out: Option<Vec<u8>> = None;

        self.lease.with_vm(&mut |lua| {
            let func = cache_fn(lua, "get")?;
            let result: Value = func
                .call(key.to_string())
                .map_err(|e| anyhow!("custom cache get error: {e:#}"))?;

            out = match result {
                Value::Nil => None,
                Value::String(s) => Some(s.as_bytes().to_vec()),
                other => bail!("custom cache get returned unexpected type: {other:?}"),
            };

            Ok(())
        })?;

        Ok(out)
    }

    fn set(&self, key: &str, value: &[u8]) -> Result<()> {
        self.lease.with_vm(&mut |lua| {
            let func = cache_fn(lua, "set")?;
            // Pass binary data as a Lua string (byte-exact, no UTF-8 demand).
            func.call::<()>((key.to_string(), lua.create_string(value)?))
                .map_err(|e| anyhow!("custom cache set error: {e:#}"))?;

            Ok(())
        })
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.lease.with_vm(&mut |lua| {
            let func = cache_fn(lua, "delete")?;
            func.call::<()>(key.to_string())
                .map_err(|e| anyhow!("custom cache delete error: {e:#}"))?;

            Ok(())
        })
    }

    fn clear(&self) -> Result<()> {
        self.lease.with_vm(&mut |lua| {
            let func = cache_fn(lua, "clear")?;
            func.call::<()>(())
                .map_err(|e| anyhow!("custom cache clear error: {e:#}"))?;

            Ok(())
        })
    }

    fn has(&self, key: &str) -> Result<bool> {
        let mut out = false;

        self.lease.with_vm(&mut |lua| {
            let tbl = cache_table(lua)?;

            // `has` is optional: fall back to a `get` probe when absent.
            out = if let Ok(Value::Function(func)) = tbl.get::<Value>("has") {
                func.call(key.to_string())
                    .map_err(|e| anyhow!("custom cache has error: {e:#}"))?
            } else {
                let get: Function = cache_fn(lua, "get")?;
                let result: Value = get
                    .call(key.to_string())
                    .map_err(|e| anyhow!("custom cache get error: {e:#}"))?;
                !matches!(result, Value::Nil)
            };

            Ok(())
        })?;

        Ok(out)
    }

    fn kind(&self) -> &'static str {
        "custom"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lua_lease::LocalLease;

    /// Create a Lua state with an in-memory cache handler registered as
    /// `crap._cache` (the shape `crap.cache.register` stores).
    fn setup_lua(with_has: bool) -> Lua {
        let lua = Lua::new();
        lua.load(format!(
            r"
            crap = {{}}
            local store = {{}}
            crap._cache = {{
                get = function(key) return store[key] end,
                set = function(key, value) store[key] = value end,
                delete = function(key) store[key] = nil end,
                clear = function() store = {{}} end,
                {}
            }}
            ",
            if with_has {
                "has = function(key) return store[key] ~= nil end,"
            } else {
                ""
            }
        ))
        .exec()
        .expect("Lua setup failed");
        lua
    }

    fn cache_over(lua: &Lua) -> CustomCache {
        CustomCache::new(Arc::new(LocalLease::new(lua)))
    }

    #[test]
    fn get_set_roundtrip_is_binary_safe() {
        let lua = setup_lua(true);
        let cache = cache_over(&lua);

        assert!(cache.get("k1").unwrap().is_none());
        cache.set("k1", b"hello\x00\xffworld").unwrap();
        assert_eq!(cache.get("k1").unwrap().unwrap(), b"hello\x00\xffworld");
        assert_eq!(cache.kind(), "custom");
    }

    #[test]
    fn delete_removes_key() {
        let lua = setup_lua(true);
        let cache = cache_over(&lua);

        cache.set("k1", b"v1").unwrap();
        cache.delete("k1").unwrap();
        assert!(cache.get("k1").unwrap().is_none());
    }

    #[test]
    fn clear_removes_all() {
        let lua = setup_lua(true);
        let cache = cache_over(&lua);

        cache.set("k1", b"v1").unwrap();
        cache.set("k2", b"v2").unwrap();
        cache.clear().unwrap();
        assert!(cache.get("k1").unwrap().is_none());
        assert!(cache.get("k2").unwrap().is_none());
    }

    #[test]
    fn has_uses_handler_or_get_fallback() {
        for with_has in [true, false] {
            let lua = setup_lua(with_has);
            let cache = cache_over(&lua);

            assert!(!cache.has("k").unwrap(), "with_has={with_has}");
            cache.set("k", b"v").unwrap();
            assert!(cache.has("k").unwrap(), "with_has={with_has}");
        }
    }

    #[test]
    fn missing_registration_errors_with_register_hint() {
        let lua = Lua::new();
        lua.load("crap = {}").exec().unwrap();
        let cache = cache_over(&lua);

        let err = cache.verify_registered().unwrap_err().to_string();
        assert!(err.contains("crap.cache.register"), "{err}");
        let err = cache.get("k").unwrap_err().to_string();
        assert!(err.contains("crap.cache.register"), "{err}");
    }

    #[test]
    fn handler_error_propagates() {
        let lua = Lua::new();
        lua.load(
            r"
            crap = { _cache = {
                get = function(key) error('backend down') end,
                set = function() end, delete = function() end, clear = function() end,
            } }
            ",
        )
        .exec()
        .unwrap();
        let cache = cache_over(&lua);

        let err = cache.get("k").unwrap_err().to_string();
        assert!(err.contains("backend down"), "{err}");
    }
}
