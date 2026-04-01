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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::upload::StorageBackend;

    /// Create a Lua state with an in-memory storage implementation.
    fn setup_lua() -> Lua {
        let lua = Lua::new();
        lua.load(
            r#"
            crap = {}
            crap._storage = {}

            -- In-memory file store
            local files = {}

            crap._storage.put = function(key, data, content_type)
                files[key] = { data = data, content_type = content_type }
            end

            crap._storage.get = function(key)
                local entry = files[key]
                if not entry then error("not found: " .. key) end
                return entry.data
            end

            crap._storage.delete = function(key)
                files[key] = nil
            end

            crap._storage.url = function(key)
                return "https://cdn.test/" .. key
            end

            crap._storage.exists = function(key)
                return files[key] ~= nil
            end
            "#,
        )
        .exec()
        .expect("Lua setup failed");
        lua
    }

    #[test]
    fn put_get_roundtrip() {
        let lua = setup_lua();
        let storage = CustomStorage::new(lua);

        storage
            .put("media/test.txt", b"hello world", "text/plain")
            .unwrap();

        let data = storage.get("media/test.txt").unwrap();
        assert_eq!(data, b"hello world");
    }

    #[test]
    fn get_nonexistent_returns_error() {
        let lua = setup_lua();
        let storage = CustomStorage::new(lua);

        let result = storage.get("nonexistent.txt");
        assert!(result.is_err());
    }

    #[test]
    fn delete_removes_file() {
        let lua = setup_lua();
        let storage = CustomStorage::new(lua);

        storage
            .put("media/file.txt", b"data", "text/plain")
            .unwrap();
        assert!(storage.exists("media/file.txt").unwrap());

        storage.delete("media/file.txt").unwrap();
        assert!(!storage.exists("media/file.txt").unwrap());
    }

    #[test]
    fn delete_nonexistent_is_ok() {
        let lua = setup_lua();
        let storage = CustomStorage::new(lua);

        // Should not error
        storage.delete("nonexistent.txt").unwrap();
    }

    #[test]
    fn exists_returns_correct_value() {
        let lua = setup_lua();
        let storage = CustomStorage::new(lua);

        assert!(!storage.exists("media/nope.txt").unwrap());

        storage.put("media/yes.txt", b"data", "text/plain").unwrap();
        assert!(storage.exists("media/yes.txt").unwrap());
    }

    #[test]
    fn public_url_delegates_to_lua() {
        let lua = setup_lua();
        let storage = CustomStorage::new(lua);

        assert_eq!(
            storage.public_url("media/photo.jpg"),
            "https://cdn.test/media/photo.jpg"
        );
    }

    #[test]
    fn kind_returns_custom() {
        let lua = setup_lua();
        let storage = CustomStorage::new(lua);
        assert_eq!(storage.kind(), "custom");
    }

    #[test]
    fn binary_data_roundtrip() {
        let lua = setup_lua();
        let storage = CustomStorage::new(lua);

        // Binary data with null bytes, high bytes, etc.
        let binary: Vec<u8> = (0..=255).collect();
        storage
            .put("media/binary.bin", &binary, "application/octet-stream")
            .unwrap();

        let result = storage.get("media/binary.bin").unwrap();
        assert_eq!(result, binary);
    }

    #[test]
    fn exists_fallback_without_exists_function() {
        let lua = Lua::new();
        lua.load(
            r#"
            crap = {}
            crap._storage = {}
            local files = {}

            crap._storage.put = function(key, data, ct)
                files[key] = data
            end
            crap._storage.get = function(key)
                if not files[key] then error("not found") end
                return files[key]
            end
            crap._storage.delete = function(key) files[key] = nil end
            crap._storage.url = function(key) return "/" .. key end
            -- No exists function — should fall back to get
            "#,
        )
        .exec()
        .expect("Lua setup failed");

        let storage = CustomStorage::new(lua);

        assert!(!storage.exists("nope.txt").unwrap());

        storage.put("yes.txt", b"data", "text/plain").unwrap();
        assert!(storage.exists("yes.txt").unwrap());
    }

    #[test]
    fn missing_storage_functions_return_error() {
        let lua = Lua::new();
        lua.load("crap = { _storage = {} }")
            .exec()
            .expect("Lua setup failed");

        let storage = CustomStorage::new(lua);

        assert!(storage.put("k", b"d", "t").is_err());
        assert!(storage.get("k").is_err());
        assert!(storage.delete("k").is_err());
    }
}
