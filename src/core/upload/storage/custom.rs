//! Custom Lua-delegated storage backend.
//!
//! Delegates all storage operations to user-provided Lua functions
//! registered via `crap.storage.register({ put, get, delete, exists })`. The
//! VM that runs the functions is supplied by a [`LuaVmLease`] — a
//! `LocalLease` when used from inside a pool VM (e.g. CRUD delete), or the
//! hook runner's pooled lease for external callers (upload-serving
//! handlers, the image-conversion job worker).
//!
//! A backend that also registers `stat` and `get_range` reports an object's
//! size and validators without reading it and hands over byte ranges, so the
//! serve route answers conditional requests from metadata and streams a body
//! in bounded slices. Without them every serve reads the whole object through
//! `get`.

use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use mlua::{Function, Lua, LuaSerdeExt, String as LuaString, Table, Value};
use serde::Deserialize;

use super::backend::validate_key;
use super::range::whole_or_slice;
use super::{ByteRange, ObjectMeta, RangedObject, StorageBackend, StorageNotFound};
use crate::{core::LuaVmLease, typegen::lua::LuaAnnotation};

/// What a custom backend's `stat` handler returns for a stored object.
/// Unknown keys are rejected.
#[derive(Debug, Deserialize, LuaAnnotation)]
#[serde(deny_unknown_fields)]
#[lua(class = "crap.StorageStat")]
pub struct StorageStat {
    /// Size of the stored object in bytes.
    pub size: u64,
    /// The object's entity tag: an opaque version string that changes
    /// whenever the object's bytes change (surrounding quotes are stripped).
    pub etag: Option<String>,
    /// When the object last changed: Unix seconds, or an HTTP-date string
    /// (`"Sun, 06 Nov 1994 08:49:37 GMT"`).
    #[lua(ty = "integer|string", optional)]
    pub last_modified: Option<StatModified>,
}

/// The two forms `StorageStat.last_modified` accepts.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum StatModified {
    /// Seconds since the Unix epoch.
    Unix(i64),
    /// An HTTP-date (RFC 2822 form).
    Date(String),
}

impl StorageStat {
    /// The metadata the serve route works from, validated.
    fn into_meta(self) -> Result<ObjectMeta> {
        let etag = self.etag.map(|tag| checked_etag(&tag)).transpose()?;
        let last_modified = self.last_modified.as_ref().map(http_date).transpose()?;

        Ok(ObjectMeta::builder()
            .size(Some(self.size))
            .etag(etag)
            .last_modified(last_modified)
            .build())
    }
}

/// An entity tag the serve route can quote: surrounding quotes stripped, then
/// non-empty visible ASCII without `"`.
fn checked_etag(tag: &str) -> Result<String> {
    let bare = tag.trim_matches('"');

    if bare.is_empty() || !bare.bytes().all(|b| b.is_ascii_graphic() && b != b'"') {
        bail!("custom storage stat: etag {tag:?} must be non-empty visible ASCII without quotes");
    }

    Ok(bare.to_string())
}

/// `last_modified` as the HTTP-date `Last-Modified` carries.
fn http_date(modified: &StatModified) -> Result<String> {
    let time: DateTime<Utc> = match modified {
        StatModified::Unix(secs) => DateTime::from_timestamp(*secs, 0)
            .ok_or_else(|| anyhow!("custom storage stat: last_modified {secs} is out of range"))?,
        StatModified::Date(date) => DateTime::parse_from_rfc2822(date.trim())
            .map_err(|e| anyhow!("custom storage stat: last_modified {date:?}: {e}"))?
            .with_timezone(&Utc),
    };

    Ok(time.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
}

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

/// The registered `crap._storage` handler table on a VM.
fn storage_table(lua: &Lua) -> Result<Table> {
    let crap: Table = lua
        .globals()
        .get("crap")
        .map_err(|e| anyhow!("crap global not found: {e}"))?;

    crap.get("_storage").map_err(|e| {
        anyhow!("crap._storage not registered — call crap.storage.register in init.lua: {e}")
    })
}

/// Look up a registered `crap._storage.<name>` function on a VM.
fn storage_fn(lua: &Lua, name: &str) -> Result<Function> {
    storage_table(lua)?
        .get(name)
        .map_err(|e| anyhow!("crap._storage.{name} not found: {e}"))
}

/// The optional ranged handlers — `stat` and `get_range` — when registered.
/// Registration accepts them only as a pair.
fn ranged_fns(lua: &Lua) -> Result<Option<(Function, Function)>> {
    let storage = storage_table(lua)?;

    let (Value::Function(stat), Value::Function(get_range)) = (
        storage.get::<Value>("stat")?,
        storage.get::<Value>("get_range")?,
    ) else {
        return Ok(None);
    };

    Ok(Some((stat, get_range)))
}

/// Call `stat(key)`: the object's metadata, or [`StorageNotFound`] for `nil`.
fn call_stat(lua: &Lua, stat: &Function, key: &str) -> Result<ObjectMeta> {
    let value: Value = stat
        .call(key.to_string())
        .map_err(|e| anyhow!("custom storage stat error: {e:#}"))?;

    if value.is_nil() {
        return Err(StorageNotFound(key.to_string()).into());
    }

    let stat: StorageStat = lua
        .from_value(value)
        .map_err(|e| anyhow!("custom storage stat returned an invalid table: {e}"))?;

    stat.into_meta()
}

/// Whether `stat` reports `key` present: `false` for its `nil`, an error for
/// a raised or invalid answer.
fn stat_says_present(lua: &Lua, stat: &Function, key: &str) -> Result<bool> {
    match call_stat(lua, stat, key) {
        Ok(_) => Ok(true),
        Err(e) if e.downcast_ref::<StorageNotFound>().is_some() => Ok(false),
        Err(e) => Err(e),
    }
}

/// Call `get_range(key, first, last)` and check it returned exactly that
/// slice.
fn call_get_range(get_range: &Function, key: &str, (first, last): (u64, u64)) -> Result<Vec<u8>> {
    let data: Option<LuaString> = get_range
        .call((key.to_string(), first, last))
        .map_err(|e| anyhow!("custom storage get_range error: {e:#}"))?;

    let Some(data) = data else {
        return Err(StorageNotFound(key.to_string()).into());
    };

    let data = data.as_bytes().to_vec();
    let expected = last - first + 1;

    if u64::try_from(data.len()).ok() != Some(expected) {
        bail!(
            "custom storage get_range returned {} bytes for the {expected}-byte range {first}-{last}",
            data.len()
        );
    }

    Ok(data)
}

/// A ranged read through the ranged handlers: `stat` resolves the range and
/// reports the object's validators, `get_range` hands over the slice.
fn read_ranged(
    lua: &Lua,
    (stat, get_range): &(Function, Function),
    key: &str,
    range: ByteRange,
) -> Result<Option<RangedObject>> {
    let meta = call_stat(lua, stat, key)?;
    let size = meta.size.unwrap_or_default();

    let Some(offsets) = range.resolve(size) else {
        return Ok(None);
    };

    let data = call_get_range(get_range, key, offsets)?;

    Ok(Some(
        RangedObject::whole(data)
            .total_size(Some(size))
            .range(Some(offsets))
            .etag(meta.etag)
            .last_modified(meta.last_modified)
            .build(),
    ))
}

impl StorageBackend for CustomStorage {
    fn put(&self, key: &str, data: &[u8], content_type: &str) -> Result<()> {
        validate_key(key)?;
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
        validate_key(key)?;
        let mut out: Option<Vec<u8>> = None;
        self.lease.with_vm(&mut |lua| {
            let func = storage_fn(lua, "get")?;
            let result: Option<LuaString> = func
                .call(key.to_string())
                .map_err(|e| anyhow!("custom storage get error: {e:#}"))?;
            out = result.map(|s| s.as_bytes().to_vec());
            Ok(())
        })?;
        out.ok_or_else(|| StorageNotFound(key.to_string()).into())
    }

    /// With the ranged handlers registered, a ranged read fetches only the
    /// slice; otherwise (and for a whole-object read) it goes through `get`.
    fn get_range(&self, key: &str, range: Option<ByteRange>) -> Result<Option<RangedObject>> {
        validate_key(key)?;

        let Some(range) = range else {
            return Ok(whole_or_slice(self.get(key)?, None));
        };

        let mut out: Option<Option<RangedObject>> = None;
        self.lease.with_vm(&mut |lua| {
            if let Some(fns) = ranged_fns(lua)? {
                out = Some(read_ranged(lua, &fns, key, range)?);
            }

            Ok(())
        })?;

        match out {
            Some(object) => Ok(object),
            None => Ok(whole_or_slice(self.get(key)?, Some(&range))),
        }
    }

    /// The `stat` handler's answer, or `None` (read through `get`) without
    /// the ranged handlers.
    fn stat(&self, key: &str) -> Result<Option<ObjectMeta>> {
        validate_key(key)?;

        let mut out = None;
        self.lease.with_vm(&mut |lua| {
            if let Some((stat, _)) = ranged_fns(lua)? {
                out = Some(call_stat(lua, &stat, key)?);
            }

            Ok(())
        })?;

        Ok(out)
    }

    fn delete(&self, key: &str) -> Result<()> {
        validate_key(key)?;
        self.lease.with_vm(&mut |lua| {
            let func = storage_fn(lua, "delete")?;
            func.call::<()>(key.to_string())
                .map_err(|e| anyhow!("custom storage delete error: {e:#}"))?;
            Ok(())
        })
    }

    fn exists(&self, key: &str) -> Result<bool> {
        // An invalid key definitionally cannot map to a stored object — return
        // false rather than erroring, matching `LocalStorage::exists`.
        if validate_key(key).is_err() {
            return Ok(false);
        }

        let mut out = false;
        self.lease.with_vm(&mut |lua| {
            // Prefer an explicit `exists`, then `stat`, then a `get` probe.
            if let Ok(func) = storage_fn(lua, "exists") {
                out = func
                    .call(key.to_string())
                    .map_err(|e| anyhow!("custom storage exists error: {e:#}"))?;
                return Ok(());
            }

            // No `exists` handler but a `stat`: ask it — a nil answer means
            // absent — without transferring the object.
            if let Some((stat, _)) = ranged_fns(lua)? {
                out = stat_says_present(lua, &stat, key)?;
                return Ok(());
            }

            // No `exists` handler: probe `get`. A nil return means absent;
            // a raised error is transient and must propagate so exists()
            // agrees with get()'s nil-vs-raise classification rather than
            // reporting a transient failure as "absent".
            out = match storage_fn(lua, "get") {
                Ok(getf) => getf
                    .call::<Option<LuaString>>(key.to_string())
                    .map_err(|e| anyhow!("custom storage exists (get probe) error: {e:#}"))?
                    .is_some(),
                Err(_) => false,
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
    use crate::core::LocalLease;
    use crate::core::upload::StorageBackend;

    /// Returns the owning `Lua` alongside a lease over a Lua state with an
    /// in-memory storage impl. The caller must keep the VM alive (the
    /// lease holds only a weak handle).
    fn setup_lease() -> (Lua, Arc<dyn LuaVmLease>) {
        let lua = Lua::new();
        lua.load(
            r"
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

            crap._storage.exists = function(key)
                return files[key] ~= nil
            end
            ",
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

    /// Regression: the storage key contract (`validate_key`) is enforced on
    /// the custom backend BEFORE dispatching to user Lua — a traversal /
    /// absolute / null-byte key is rejected at the boundary, so a user
    /// `put`/`get` handler that maps keys onto a filesystem can never be
    /// handed an escaping key. Keeps all three backends in parity.
    #[test]
    fn rejects_malformed_keys_before_dispatching_to_lua() {
        let (_lua, lease) = setup_lease();
        let storage = CustomStorage::new(lease);

        assert!(storage.put("../escape.txt", b"x", "text/plain").is_err());
        assert!(storage.get("../escape.txt").is_err());
        assert!(storage.delete("../escape.txt").is_err());
        assert!(storage.put("/etc/passwd", b"x", "text/plain").is_err());
        assert!(storage.put("ok\0hidden", b"x", "text/plain").is_err());

        // exists() maps an invalid key to "not present", matching LocalStorage.
        assert!(!storage.exists("../escape.txt").unwrap());
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
            r"
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
            -- No exists function — should fall back to get
            ",
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

    /// A backend with the ranged handlers over one ten-byte object. It counts
    /// `get` calls in `crap._gets`; `stat` returns `crap._stat`'s override
    /// when one is set.
    const RANGED_BACKEND: &str = r#"
        crap = { _storage = {}, _gets = 0 }
        local files = { ["media/a.bin"] = "0123456789" }

        crap._storage.put = function(key, data) files[key] = data end
        crap._storage.delete = function(key) files[key] = nil end

        crap._storage.get = function(key)
            crap._gets = crap._gets + 1
            return files[key]
        end

        crap._storage.stat = function(key)
            if crap._stat then return crap._stat end

            local data = files[key]
            if not data then return nil end

            return { size = #data, etag = '"v1"', last_modified = 784111777 }
        end

        crap._storage.get_range = function(key, first, last)
            local data = files[key]
            if not data then return nil end

            if crap._short then return data:sub(first + 1, last) end

            return data:sub(first + 1, last + 1)
        end
    "#;

    fn ranged_storage() -> (Lua, CustomStorage) {
        let lua = Lua::new();
        lua.load(RANGED_BACKEND).exec().unwrap();
        let storage = CustomStorage::new(Arc::new(LocalLease::new(&lua)));

        (lua, storage)
    }

    fn gets(lua: &Lua) -> i64 {
        lua.load("return crap._gets").eval().unwrap()
    }

    #[test]
    fn stat_reports_the_size_and_validators() {
        let (_lua, storage) = ranged_storage();

        let meta = storage.stat("media/a.bin").unwrap().expect("metadata");

        assert_eq!(meta.size, Some(10));
        assert_eq!(meta.etag.as_deref(), Some("v1"));
        assert_eq!(
            meta.last_modified.as_deref(),
            Some("Sun, 06 Nov 1994 08:49:37 GMT")
        );
    }

    /// The ranged handlers serve a slice without ever reading the whole
    /// object through `get`.
    #[test]
    fn a_ranged_read_uses_get_range_not_get() {
        let (lua, storage) = ranged_storage();

        let slice = storage
            .get_range("media/a.bin", Some(ByteRange::inclusive(2, 4)))
            .unwrap()
            .expect("satisfiable");
        assert_eq!(slice.data, b"234");
        assert_eq!(slice.range, Some((2, 4)));
        assert_eq!(slice.total_size, Some(10));
        assert_eq!(slice.etag.as_deref(), Some("v1"));

        let tail = storage
            .get_range("media/a.bin", Some(ByteRange::Suffix(3)))
            .unwrap()
            .expect("satisfiable");
        assert_eq!(tail.data, b"789");

        assert!(
            storage
                .get_range("media/a.bin", Some(ByteRange::from_start(10)))
                .unwrap()
                .is_none(),
            "a range past the end is unsatisfiable"
        );

        assert_eq!(gets(&lua), 0);
    }

    #[test]
    fn a_missing_object_is_not_found_on_both_ranged_handlers() {
        let (_lua, storage) = ranged_storage();

        let stat = storage.stat("media/none.bin").unwrap_err();
        assert!(stat.downcast_ref::<StorageNotFound>().is_some(), "{stat:#}");

        let read = storage
            .get_range("media/none.bin", Some(ByteRange::inclusive(0, 1)))
            .unwrap_err();
        assert!(read.downcast_ref::<StorageNotFound>().is_some(), "{read:#}");
    }

    /// A `get_range` that hands back a different number of bytes than asked
    /// is a failure, never a silently short (or long) slice.
    #[test]
    fn a_short_slice_is_an_error() {
        let (lua, storage) = ranged_storage();
        lua.load("crap._short = true").exec().unwrap();

        let err = storage
            .get_range("media/a.bin", Some(ByteRange::inclusive(2, 4)))
            .unwrap_err();

        assert!(err.downcast_ref::<StorageNotFound>().is_none());
        assert!(format!("{err:#}").contains("returned 2 bytes"), "{err:#}");
    }

    #[test]
    fn an_invalid_stat_table_is_an_error() {
        let (lua, storage) = ranged_storage();

        for bad in [
            "{ size = 10, sise = 1 }",
            "{ etag = 'v1' }",
            "{ size = -1 }",
            "{ size = 10, etag = 'a\"b' }",
            "{ size = 10, etag = '' }",
            "{ size = 10, last_modified = 'yesterday' }",
        ] {
            lua.load(format!("crap._stat = {bad}")).exec().unwrap();

            let err = storage.stat("media/a.bin").unwrap_err();
            assert!(err.downcast_ref::<StorageNotFound>().is_none(), "{bad}");
        }
    }

    #[test]
    fn an_http_date_last_modified_is_normalized() {
        let (lua, storage) = ranged_storage();
        lua.load("crap._stat = { size = 10, last_modified = 'Sun, 6 Nov 1994 09:49:37 +0100' }")
            .exec()
            .unwrap();

        let meta = storage.stat("media/a.bin").unwrap().expect("metadata");

        assert_eq!(
            meta.last_modified.as_deref(),
            Some("Sun, 06 Nov 1994 08:49:37 GMT")
        );
    }

    /// Without an `exists` handler, `stat` answers existence instead of a
    /// `get` probe that would download the whole object.
    #[test]
    fn exists_asks_stat_before_probing_get() {
        let (lua, storage) = ranged_storage();

        assert!(storage.exists("media/a.bin").unwrap());
        assert!(!storage.exists("media/none.bin").unwrap());
        assert_eq!(gets(&lua), 0);
    }

    /// Without the ranged handlers the backend reports no metadata, and a
    /// ranged read slices what `get` returned.
    #[test]
    fn without_ranged_handlers_reads_go_through_get() {
        let (_lua, lease) = setup_lease();
        let storage = CustomStorage::new(lease);
        storage
            .put("media/b.txt", b"0123456789", "text/plain")
            .unwrap();

        assert!(storage.stat("media/b.txt").unwrap().is_none());

        let slice = storage
            .get_range("media/b.txt", Some(ByteRange::inclusive(1, 2)))
            .unwrap()
            .expect("satisfiable");
        assert_eq!(slice.data, b"12");
    }
}
