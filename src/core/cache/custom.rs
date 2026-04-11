//! Custom Lua-delegated cache backend.
//!
//! Delegates all cache operations to user-provided Lua functions
//! registered via `crap.cache.register({ get, set, delete, clear, has })`.

use anyhow::{Result, anyhow, bail};
use mlua::{Function, Lua};

use crate::core::cache::CacheBackend;

/// Custom cache backend that delegates to Lua functions.
///
/// The Lua functions are stored in `crap._cache` and called
/// synchronously via the hook runner's Lua VM pool.
pub struct CustomCache {
    lua: Lua,
}

impl CustomCache {
    /// Create a new custom cache backend.
    /// The Lua state must have `crap._cache` functions registered.
    pub fn new(lua: Lua) -> Self {
        Self { lua }
    }

    /// Get a registered cache function from the Lua state.
    fn get_fn(&self, name: &str) -> Result<Function> {
        let crap: mlua::Table = self
            .lua
            .globals()
            .get("crap")
            .map_err(|e| anyhow!("crap global not found: {}", e))?;

        let cache: mlua::Table = crap
            .get("_cache")
            .map_err(|e| anyhow!("crap._cache not found: {}", e))?;

        cache
            .get(name)
            .map_err(|e| anyhow!("crap._cache.{} not found: {}", name, e))
    }
}

impl CacheBackend for CustomCache {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let func = self.get_fn("get")?;
        let result: mlua::Value = func
            .call(key.to_string())
            .map_err(|e| anyhow!("custom cache get error: {:#}", e))?;

        match result {
            mlua::Value::Nil => Ok(None),
            mlua::Value::String(s) => Ok(Some(s.as_bytes().to_vec())),
            other => bail!("custom cache get returned unexpected type: {:?}", other),
        }
    }

    fn set(&self, key: &str, value: &[u8]) -> Result<()> {
        let func = self.get_fn("set")?;

        func.call::<()>((key.to_string(), self.lua.create_string(value)?))
            .map_err(|e| anyhow!("custom cache set error: {:#}", e))
    }

    fn delete(&self, key: &str) -> Result<()> {
        let func = self.get_fn("delete")?;

        func.call::<()>(key.to_string())
            .map_err(|e| anyhow!("custom cache delete error: {:#}", e))
    }

    fn clear(&self) -> Result<()> {
        let func = self.get_fn("clear")?;

        func.call::<()>(())
            .map_err(|e| anyhow!("custom cache clear error: {:#}", e))
    }

    fn has(&self, key: &str) -> Result<bool> {
        match self.get_fn("has") {
            Ok(func) => {
                let result: bool = func
                    .call(key.to_string())
                    .map_err(|e| anyhow!("custom cache has error: {:#}", e))?;
                Ok(result)
            }
            Err(_) => {
                // Fallback: use get and check for nil
                Ok(self.get(key)?.is_some())
            }
        }
    }

    fn kind(&self) -> &'static str {
        "custom"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::cache::CacheBackend;

    /// Create a Lua state with an in-memory cache implementation.
    fn setup_lua() -> Lua {
        let lua = Lua::new();
        lua.load(
            r#"
            crap = {}
            crap._cache = {}

            local store = {}

            crap._cache.get = function(key)
                return store[key]
            end

            crap._cache.set = function(key, value)
                store[key] = value
            end

            crap._cache.delete = function(key)
                store[key] = nil
            end

            crap._cache.clear = function()
                store = {}
            end

            crap._cache.has = function(key)
                return store[key] ~= nil
            end
            "#,
        )
        .exec()
        .expect("Lua setup failed");
        lua
    }

    #[test]
    fn get_set_roundtrip() {
        let lua = setup_lua();
        let cache = CustomCache::new(lua);

        assert!(cache.get("k1").unwrap().is_none());

        cache.set("k1", b"hello").unwrap();
        assert_eq!(cache.get("k1").unwrap().unwrap(), b"hello");
    }

    #[test]
    fn delete_removes_key() {
        let lua = setup_lua();
        let cache = CustomCache::new(lua);

        cache.set("k1", b"v1").unwrap();
        cache.delete("k1").unwrap();
        assert!(cache.get("k1").unwrap().is_none());
    }

    #[test]
    fn clear_removes_all() {
        let lua = setup_lua();
        let cache = CustomCache::new(lua);

        cache.set("k1", b"v1").unwrap();
        cache.set("k2", b"v2").unwrap();
        cache.clear().unwrap();
        assert!(cache.get("k1").unwrap().is_none());
        assert!(cache.get("k2").unwrap().is_none());
    }

    #[test]
    fn has_returns_correct_state() {
        let lua = setup_lua();
        let cache = CustomCache::new(lua);

        assert!(!cache.has("k1").unwrap());
        cache.set("k1", b"v1").unwrap();
        assert!(cache.has("k1").unwrap());
    }

    #[test]
    fn kind_returns_custom() {
        let lua = setup_lua();
        let cache = CustomCache::new(lua);
        assert_eq!(cache.kind(), "custom");
    }

    #[test]
    fn binary_data_roundtrip() {
        let lua = setup_lua();
        let cache = CustomCache::new(lua);

        let binary: Vec<u8> = (0..=255).collect();
        cache.set("bin", &binary).unwrap();
        assert_eq!(cache.get("bin").unwrap().unwrap(), binary);
    }

    #[test]
    fn has_fallback_without_has_function() {
        let lua = Lua::new();
        lua.load(
            r#"
            crap = {}
            crap._cache = {}
            local store = {}

            crap._cache.get = function(key) return store[key] end
            crap._cache.set = function(key, value) store[key] = value end
            crap._cache.delete = function(key) store[key] = nil end
            crap._cache.clear = function() store = {} end
            -- No has function — should fall back to get
            "#,
        )
        .exec()
        .expect("Lua setup failed");

        let cache = CustomCache::new(lua);
        assert!(!cache.has("nope").unwrap());

        cache.set("yes", b"data").unwrap();
        assert!(cache.has("yes").unwrap());
    }

    #[test]
    fn missing_cache_functions_return_error() {
        let lua = Lua::new();
        lua.load("crap = { _cache = {} }")
            .exec()
            .expect("Lua setup failed");

        let cache = CustomCache::new(lua);
        assert!(cache.set("k", b"v").is_err());
        assert!(cache.delete("k").is_err());
        assert!(cache.clear().is_err());
    }
}
