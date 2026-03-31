//! Storage backend abstraction for upload files.
//!
//! Provides a trait-based backend system: `local` (default filesystem),
//! `s3` (S3-compatible, feature-flagged), and `custom` (Lua-delegated).

mod custom;
mod local;
#[cfg(feature = "s3-storage")]
mod s3;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

pub use custom::CustomStorage;
pub use local::LocalStorage;

/// Thread-safe shared reference to a storage backend.
pub type SharedStorage = Arc<dyn StorageBackend>;

/// Object-safe storage backend trait.
///
/// Keys are forward-slash separated paths like `media/abc123_photo.jpg`.
/// Implementations handle the mapping to their native addressing (filesystem
/// paths, S3 object keys, etc.).
pub trait StorageBackend: Send + Sync {
    /// Store a file. Overwrites if the key already exists.
    fn put(&self, key: &str, data: &[u8], content_type: &str) -> Result<()>;

    /// Retrieve a file's contents.
    fn get(&self, key: &str) -> Result<Vec<u8>>;

    /// Delete a file. No error if the key doesn't exist.
    fn delete(&self, key: &str) -> Result<()>;

    /// Check whether a key exists.
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
    /// with Range, ETag, and conditional GET support.
    /// Non-local backends return `None` and files are served via `get()`.
    fn local_path(&self, key: &str) -> Option<std::path::PathBuf> {
        let _ = key;
        None
    }
}

/// Create the appropriate storage backend from config.
pub fn create_storage(
    config_dir: &Path,
    config: &crate::config::UploadConfig,
) -> Result<SharedStorage> {
    match config.storage.as_str() {
        "local" | "" => {
            let base_dir = config_dir.join("uploads");
            Ok(Arc::new(LocalStorage::new(base_dir)))
        }
        #[cfg(feature = "s3-storage")]
        "s3" => s3::create_s3_storage(&config.s3),
        "custom" => {
            todo!("Custom Lua storage backend not yet implemented")
        }
        other => anyhow::bail!("Unknown upload storage backend: '{}'", other),
    }
}
