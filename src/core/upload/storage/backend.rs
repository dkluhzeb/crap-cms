//! `StorageBackend` trait + `SharedStorage` type alias — the
//! abstraction every upload-storage backend in this module satisfies.

use std::{fmt, path::PathBuf, sync::Arc};

use anyhow::Result;

/// Thread-safe shared reference to a storage backend.
pub type SharedStorage = Arc<dyn StorageBackend>;

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
