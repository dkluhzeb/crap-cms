//! Custom Lua-delegated storage backend.
//!
//! Delegates all storage operations to user-provided Lua functions
//! registered via `crap.storage.register({ put, get, delete, url })`.

use anyhow::Result;
use mlua::{Function, Lua};

use super::StorageBackend;

/// Custom storage backend that delegates to Lua functions.
///
/// The Lua functions are stored in the Lua registry and called
/// synchronously via the hook runner's Lua VM pool.
pub struct CustomStorage {
    lua: Lua,
}

impl CustomStorage {
    /// Create a new custom storage backend.
    /// The Lua state must have `crap.storage` functions registered.
    pub fn new(lua: Lua) -> Self {
        Self { lua }
    }

    /// Get a registered storage function from the Lua state.
    fn get_fn(&self, name: &str) -> Result<Function> {
        let crap: mlua::Table = self
            .lua
            .globals()
            .get("crap")
            .map_err(|e| anyhow::anyhow!("crap global not found: {}", e))?;

        let storage: mlua::Table = crap
            .get("_storage")
            .map_err(|e| anyhow::anyhow!("crap._storage not found: {}", e))?;

        storage
            .get(name)
            .map_err(|e| anyhow::anyhow!("crap._storage.{} not found: {}", name, e))
    }
}

impl StorageBackend for CustomStorage {
    fn put(&self, key: &str, data: &[u8], content_type: &str) -> Result<()> {
        let func = self.get_fn("put")?;
        // Pass binary data as Lua string (mlua handles Vec<u8> <-> Lua string natively)
        func.call::<()>((
            key.to_string(),
            self.lua.create_string(data)?,
            content_type.to_string(),
        ))
        .map_err(|e| anyhow::anyhow!("custom storage put error: {:#}", e))
    }

    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let func = self.get_fn("get")?;
        let result: mlua::String = func
            .call(key.to_string())
            .map_err(|e| anyhow::anyhow!("custom storage get error: {:#}", e))?;
        Ok(result.as_bytes().to_vec())
    }

    fn delete(&self, key: &str) -> Result<()> {
        let func = self.get_fn("delete")?;
        func.call::<()>(key.to_string())
            .map_err(|e| anyhow::anyhow!("custom storage delete error: {:#}", e))
    }

    fn exists(&self, key: &str) -> Result<bool> {
        // If no exists function registered, fall back to trying get
        match self.get_fn("exists") {
            Ok(func) => {
                let result: bool = func
                    .call(key.to_string())
                    .map_err(|e| anyhow::anyhow!("custom storage exists error: {:#}", e))?;
                Ok(result)
            }
            Err(_) => {
                // No exists function — try get and check for error
                match self.get(key) {
                    Ok(_) => Ok(true),
                    Err(_) => Ok(false),
                }
            }
        }
    }

    fn public_url(&self, key: &str) -> String {
        match self.get_fn("url") {
            Ok(func) => func
                .call::<String>(key.to_string())
                .unwrap_or_else(|_| format!("/uploads/{}", key)),
            Err(_) => format!("/uploads/{}", key),
        }
    }

    fn kind(&self) -> &'static str {
        "custom"
    }
}
