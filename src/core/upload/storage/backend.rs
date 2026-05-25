//! `StorageBackend` trait + `SharedStorage` type alias — the
//! abstraction every upload-storage backend in this module satisfies.

use std::{path::PathBuf, sync::Arc};

use anyhow::Result;

/// Thread-safe shared reference to a storage backend.
pub type SharedStorage = Arc<dyn StorageBackend>;

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
    /// Returns an error if the key is missing or the backend fails.
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
