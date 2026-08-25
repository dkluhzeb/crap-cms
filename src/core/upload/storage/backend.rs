//! `StorageBackend` trait + `SharedStorage` type alias — the
//! abstraction every upload-storage backend in this module satisfies.

use std::{fmt, path::PathBuf, sync::Arc};

use anyhow::{Result, bail};

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

    /// Return the public-facing URL for a key.
    ///
    /// For local storage: `/uploads/{key}`
    /// For S3: `https://bucket.s3.region.amazonaws.com/{key}` or CDN URL
    fn public_url(&self, key: &str) -> String;

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
    use super::validate_key;

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
