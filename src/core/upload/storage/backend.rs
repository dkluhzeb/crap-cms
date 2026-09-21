//! `StorageBackend` trait + `SharedStorage` type alias — the
//! abstraction every upload-storage backend in this module satisfies.

use std::{fmt, path::PathBuf, sync::Arc};

use anyhow::{Result, bail};

use crate::core::upload::storage::range::{ByteRange, RangedObject, slice_locally};

/// Thread-safe shared reference to a storage backend.
pub type SharedStorage = Arc<dyn StorageBackend>;

/// Strict validation for storage keys — the shared key contract every backend
/// enforces at the storage trust boundary. Rejects any input that could, when
/// mapped to a native address, escape its container or otherwise be malformed:
/// path traversal via `..`, absolute paths, backslash separators, or null
/// bytes.
///
/// The trait is the trust boundary: callers (admin handlers, Lua hooks, future
/// migrations) sanitize filenames upstream, but each backend re-checks here so
/// a future caller — or a user-provided [`custom`](super::custom) Lua backend
/// that maps keys onto a filesystem — cannot accidentally escape.
///
/// # Errors
///
/// Returns an error describing the first violation found.
pub(super) fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() {
        bail!("Storage key is empty");
    }

    if key.contains('\0') {
        bail!("Storage key contains a null byte");
    }

    // Absolute paths (Unix `/` or Windows drive-letter / UNC-style) must be
    // rejected — joining an absolute RHS silently replaces the base. Checking
    // the first byte handles both forms portably.
    let first = key.as_bytes()[0];
    if first == b'/' || first == b'\\' {
        bail!("Storage key must be relative: {key:?}");
    }

    // Reject `..` as any component, using both separators so that a key like
    // `foo\..\bar` is caught on filesystems that treat `\` specially.
    for component in key.split(['/', '\\']) {
        if component == ".." {
            bail!("Storage key contains '..' traversal: {key:?}");
        }
    }

    Ok(())
}

/// Error returned by [`StorageBackend::get`] when a key genuinely does not
/// exist — as opposed to a transient/infrastructure failure (network
/// error, pool-acquire timeout, permission error, …).
///
/// Backends return this (via `anyhow::Error`) for a confirmed miss;
/// callers `downcast_ref::<StorageNotFound>()` to tell "missing" (serve a
/// 404) from "try again" (serve a 503). Anything that is *not* a
/// `StorageNotFound` is treated as transient.
#[derive(Debug)]
pub struct StorageNotFound(pub String);

impl fmt::Display for StorageNotFound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "storage key not found: {}", self.0)
    }
}

impl std::error::Error for StorageNotFound {}

/// Object-safe storage backend trait.
///
/// Keys are forward-slash separated paths like `media/abc123_photo.jpg`.
/// Implementations handle the mapping to their native addressing (filesystem
/// paths, S3 object keys, etc.).
pub trait StorageBackend: Send + Sync {
    /// Store a file. Overwrites if the key already exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails (IO error, network error, permission denied, …).
    fn put(&self, key: &str, data: &[u8], content_type: &str) -> Result<()>;

    /// Retrieve a file's contents.
    ///
    /// # Errors
    ///
    /// Returns [`StorageNotFound`] (wrapped in `anyhow::Error`) when the
    /// key genuinely does not exist, and any other error for a transient
    /// or infrastructure failure. Callers distinguish the two by
    /// downcasting.
    fn get(&self, key: &str) -> Result<Vec<u8>>;

    /// Retrieve a file's contents, or only the requested byte range of them.
    ///
    /// The default implementation reads the whole object through
    /// [`get`](Self::get) and slices locally, so a backend that only knows how
    /// to hand over whole files — the Lua [`custom`](super::custom) backend,
    /// whose bytes contract must not change — keeps working untouched. A
    /// backend that can push the range down to its remote (S3 sends an HTTP
    /// `Range` header) overrides this so a ranged request never transfers the
    /// whole object.
    ///
    /// `Ok(None)` means the range cannot be satisfied against this object —
    /// the caller answers `416`. That is not a failure, and not a miss.
    ///
    /// # Errors
    ///
    /// Same contract as [`get`](Self::get): [`StorageNotFound`] for a
    /// confirmed miss, any other error for a transient or infrastructure
    /// failure.
    fn get_range(&self, key: &str, range: Option<ByteRange>) -> Result<Option<RangedObject>> {
        let data = self.get(key)?;

        let Some(range) = range else {
            return Ok(Some(RangedObject::whole(data).build()));
        };

        Ok(slice_locally(&data, &range))
    }

    /// Delete a file. No error if the key doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails (IO error, permission denied, …).
    fn delete(&self, key: &str) -> Result<()>;

    /// Check whether a key exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails.
    fn exists(&self, key: &str) -> Result<bool>;

    /// Return the backend identifier (`"local"`, `"s3"`, `"custom"`).
    fn kind(&self) -> &'static str;

    /// Return the local filesystem path for a key, if this is a local backend.
    /// Used by the file serving handler to leverage `tower_http::ServeFile`
    /// with Range, `ETag`, and conditional GET support.
    /// Non-local backends return `None` and files are served via `get()`.
    fn local_path(&self, key: &str) -> Option<PathBuf> {
        let _ = key;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{ByteRange, Result, StorageBackend, validate_key};

    /// A backend that only knows how to hand over whole files — the shape the
    /// Lua `custom` backend has. It must get ranged reads for free.
    struct WholeFileBackend(Vec<u8>);

    impl StorageBackend for WholeFileBackend {
        fn put(&self, _key: &str, _data: &[u8], _content_type: &str) -> Result<()> {
            Ok(())
        }

        fn get(&self, _key: &str) -> Result<Vec<u8>> {
            Ok(self.0.clone())
        }

        fn delete(&self, _key: &str) -> Result<()> {
            Ok(())
        }

        fn exists(&self, _key: &str) -> Result<bool> {
            Ok(true)
        }

        fn kind(&self) -> &'static str {
            "whole-file"
        }
    }

    #[test]
    fn the_default_ranged_read_slices_a_whole_file_backend() {
        let backend = WholeFileBackend(b"0123456789".to_vec());

        let object = backend
            .get_range("k", Some(ByteRange::inclusive(3, 5)))
            .unwrap()
            .expect("satisfiable");

        assert_eq!(object.data, b"345");
        assert_eq!(object.range, Some((3, 5)));
        assert_eq!(object.total_size, Some(10));
    }

    #[test]
    fn the_default_ranged_read_returns_the_whole_object_without_a_range() {
        let backend = WholeFileBackend(b"0123456789".to_vec());

        let object = backend.get_range("k", None).unwrap().expect("present");

        assert_eq!(object.data.len(), 10);
        assert!(object.range.is_none());
    }

    #[test]
    fn the_default_ranged_read_reports_an_unsatisfiable_range() {
        let backend = WholeFileBackend(b"0123456789".to_vec());

        assert!(
            backend
                .get_range("k", Some(ByteRange::from_start(10)))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn accepts_well_formed_keys() {
        assert!(validate_key("media/abc123_photo.jpg").is_ok());
        assert!(validate_key("posts/thumb/small.png").is_ok());
    }

    #[test]
    fn rejects_traversal_absolute_null_and_backslash() {
        assert!(validate_key("").is_err());
        assert!(validate_key("../escape.txt").is_err());
        assert!(validate_key("a/../../escape.txt").is_err());
        assert!(validate_key("/etc/passwd").is_err());
        assert!(validate_key("\\absolute\\win.txt").is_err());
        assert!(validate_key("foo\\..\\escape.txt").is_err());
        assert!(validate_key("ok\0hidden").is_err());
    }
}
