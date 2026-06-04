//! Custom Lua-delegated storage backend.
//!
//! Delegates all storage operations to user-provided Lua functions
//! registered via `crap.storage.register({ put, get, delete, url })`. The
//! VM that runs the functions is supplied by a [`LuaVmLease`] — a
//! `LocalLease` when used from inside a pool VM (e.g. CRUD delete), or the
//! hook runner's pooled lease for external callers (upload-serving
//! handlers, the image-conversion job worker).

use std::sync::Arc;

use anyhow::{Result, anyhow};
use mlua::{Function, Lua, Table};

use super::{StorageBackend, StorageNotFound};
use crate::core::lua_lease::LuaVmLease;

/// Custom storage backend that delegates to Lua functions.
pub struct CustomStorage {
    lease: Arc<dyn LuaVmLease>,
}

impl CustomStorage {
    /// Create a new custom storage backend backed by `lease`. The leased
    /// VM must have `crap._storage` registered (via `init.lua`).
    #[must_use]
    pub fn new(lease: Arc<dyn LuaVmLease>) -> Self {
        Self { lease }
    }
}

/// Look up a registered `crap._storage.<name>` function on a VM.
fn storage_fn(lua: &Lua, name: &str) -> Result<Function> {
    let crap: Table = lua
        .globals()
        .get("crap")
        .map_err(|e| anyhow!("crap global not found: {e}"))?;

    let storage: Table = crap.get("_storage").map_err(|e| {
        anyhow!("crap._storage not registered — call crap.storage.register in init.lua: {e}")
    })?;

    storage
        .get(name)
        .map_err(|e| anyhow!("crap._storage.{name} not found: {e}"))
}

impl StorageBackend for CustomStorage {
    fn put(&self, key: &str, data: &[u8], content_type: &str) -> Result<()> {
        self.lease.with_vm(&mut |lua| {
            let func = storage_fn(lua, "put")?;
            // Pass binary data as a Lua string (mlua maps Vec<u8> <-> Lua string).
            func.call::<()>((
                key.to_string(),
                lua.create_string(data)?,
                content_type.to_string(),
            ))
            .map_err(|e| anyhow!("custom storage put error: {e:#}"))?;
            Ok(())
        })
    }

    fn get(&self, key: &str) -> Result<Vec<u8>> {
        // Contract: the handler returns the bytes (string) on hit, `nil`
        // for a missing key, and *raises* only on a real failure. So a
        // `nil` return maps to `StorageNotFound` (→ 404) while a raised
        // error or a lease failure (e.g. pool-acquire timeout) propagates
        // as a transient error (→ 503).
        let mut out: Option<Vec<u8>> = None;
        self.lease.with_vm(&mut |lua| {
            let func = storage_fn(lua, "get")?;
            let result: Option<mlua::String> = func
                .call(key.to_string())
                .map_err(|e| anyhow!("custom storage get error: {e:#}"))?;
            out = result.map(|s| s.as_bytes().to_vec());
            Ok(())
        })?;
        out.ok_or_else(|| StorageNotFound(key.to_string()).into())
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.lease.with_vm(&mut |lua| {
            let func = storage_fn(lua, "delete")?;
            func.call::<()>(key.to_string())
                .map_err(|e| anyhow!("custom storage delete error: {e:#}"))?;
            Ok(())
        })
    }

    fn exists(&self, key: &str) -> Result<bool> {
        let mut out = false;
        self.lease.with_vm(&mut |lua| {
            // Prefer an explicit `exists`; otherwise fall back to probing `get`.
            if let Ok(func) = storage_fn(lua, "exists") {
                out = func
                    .call(key.to_string())
                    .map_err(|e| anyhow!("custom storage exists error: {e:#}"))?;
                return Ok(());
            }

            // No `exists` handler: probe `get`. A nil return means absent;
            // a raised error is transient and must propagate so exists()
            // agrees with get()'s nil-vs-raise classification rather than
            // reporting a transient failure as "absent".
            out = match storage_fn(lua, "get") {
                Ok(getf) => getf
                    .call::<Option<mlua::String>>(key.to_string())
                    .map_err(|e| anyhow!("custom storage exists (get probe) error: {e:#}"))?
                    .is_some(),
                Err(_) => false,
            };
            Ok(())
        })?;
        Ok(out)
    }

    fn public_url(&self, key: &str) -> String {
        // `public_url` can't return an error, so fall back to the served
        // path on any failure. A lease failure (e.g. pool-acquire timeout)
        // is logged rather than silently swallowed, to aid diagnosis.
        let mut url = format!("/uploads/{key}");
        if let Err(e) = self.lease.with_vm(&mut |lua| {
            if let Ok(func) = storage_fn(lua, "url")
                && let Ok(u) = func.call::<String>(key.to_string())
            {
                url = u;
            }
            Ok(())
        }) {
            tracing::debug!("custom storage public_url lease failed for '{key}': {e:#}");
        }
        url
    }

    fn kind(&self) -> &'static str {
        "custom"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::lua_lease::LocalLease;
    use crate::core::upload::StorageBackend;

    /// Returns the owning `Lua` alongside a lease over a Lua state with an
    /// in-memory storage impl. The caller must keep the VM alive (the
    /// lease holds only a weak handle).
    fn setup_lease() -> (Lua, Arc<dyn LuaVmLease>) {
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
                if not entry then return nil end
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
        let lease: Arc<dyn LuaVmLease> = Arc::new(LocalLease::new(&lua));
        (lua, lease)
    }

    #[test]
    fn put_get_roundtrip() {
        let (_lua, lease) = setup_lease();
        let storage = CustomStorage::new(lease);

        storage
            .put("media/test.txt", b"hello world", "text/plain")
            .unwrap();

        let data = storage.get("media/test.txt").unwrap();
        assert_eq!(data, b"hello world");
    }

    #[test]
    fn get_missing_returns_not_found() {
        let (_lua, lease) = setup_lease();
        let storage = CustomStorage::new(lease);

        // Handler returns nil → typed StorageNotFound (so callers serve 404).
        let err = storage.get("nonexistent.txt").unwrap_err();
        assert!(
            err.downcast_ref::<StorageNotFound>().is_some(),
            "nil return must map to StorageNotFound, got: {err:#}"
        );
    }

    #[test]
    fn get_handler_error_is_transient_not_not_found() {
        // A handler that *raises* signals a real failure, not a miss —
        // it must NOT be classified as StorageNotFound (callers serve 503).
        let lua = Lua::new();
        lua.load(
            r#"
            crap = { _storage = {} }
            crap._storage.get = function(key) error("backend exploded") end
            "#,
        )
        .exec()
        .unwrap();
        let storage = CustomStorage::new(Arc::new(LocalLease::new(&lua)));

        let err = storage.get("any.txt").unwrap_err();
        assert!(
            err.downcast_ref::<StorageNotFound>().is_none(),
            "a raised handler error must be transient, not StorageNotFound"
        );
    }

    #[test]
    fn delete_removes_file() {
        let (_lua, lease) = setup_lease();
        let storage = CustomStorage::new(lease);

        storage
            .put("media/file.txt", b"data", "text/plain")
            .unwrap();
        assert!(storage.exists("media/file.txt").unwrap());

        storage.delete("media/file.txt").unwrap();
        assert!(!storage.exists("media/file.txt").unwrap());
    }

    #[test]
    fn delete_nonexistent_is_ok() {
        let (_lua, lease) = setup_lease();
        let storage = CustomStorage::new(lease);

        // Should not error
        storage.delete("nonexistent.txt").unwrap();
    }

    #[test]
    fn exists_returns_correct_value() {
        let (_lua, lease) = setup_lease();
        let storage = CustomStorage::new(lease);

        assert!(!storage.exists("media/nope.txt").unwrap());

        storage.put("media/yes.txt", b"data", "text/plain").unwrap();
        assert!(storage.exists("media/yes.txt").unwrap());
    }

    #[test]
    fn public_url_delegates_to_lua() {
        let (_lua, lease) = setup_lease();
        let storage = CustomStorage::new(lease);

        assert_eq!(
            storage.public_url("media/photo.jpg"),
            "https://cdn.test/media/photo.jpg"
        );
    }

    #[test]
    fn kind_returns_custom() {
        let (_lua, lease) = setup_lease();
        let storage = CustomStorage::new(lease);
        assert_eq!(storage.kind(), "custom");
    }

    #[test]
    fn binary_data_roundtrip() {
        let (_lua, lease) = setup_lease();
        let storage = CustomStorage::new(lease);

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
                if not files[key] then return nil end
                return files[key]
            end
            crap._storage.delete = function(key) files[key] = nil end
            crap._storage.url = function(key) return "/" .. key end
            -- No exists function — should fall back to get
            "#,
        )
        .exec()
        .expect("Lua setup failed");

        let storage = CustomStorage::new(Arc::new(LocalLease::new(&lua)));

        assert!(!storage.exists("nope.txt").unwrap());

        storage.put("yes.txt", b"data", "text/plain").unwrap();
        assert!(storage.exists("yes.txt").unwrap());
    }

    /// Regression: with no `exists` handler, a `get`-probe that *raises*
    /// (a transient failure) must propagate as an error, not be reported as
    /// a confident "absent" — keeping `exists()` consistent with `get()`.
    #[test]
    fn exists_fallback_propagates_transient_error() {
        let lua = Lua::new();
        lua.load(
            r#"
            crap = { _storage = {} }
            crap._storage.get = function(key) error("backend down") end
            "#,
        )
        .exec()
        .unwrap();
        let storage = CustomStorage::new(Arc::new(LocalLease::new(&lua)));

        assert!(storage.exists("any.txt").is_err());
    }

    #[test]
    fn missing_storage_functions_return_error() {
        let lua = Lua::new();
        lua.load("crap = { _storage = {} }")
            .exec()
            .expect("Lua setup failed");

        let storage = CustomStorage::new(Arc::new(LocalLease::new(&lua)));

        assert!(storage.put("k", b"d", "t").is_err());
        assert!(storage.get("k").is_err());
        assert!(storage.delete("k").is_err());
    }
}
