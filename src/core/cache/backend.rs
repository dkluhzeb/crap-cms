//! `CacheBackend` trait + `SharedCache` type alias — the abstraction
//! every cache implementation in this module satisfies.

use std::sync::Arc;

use anyhow::Result;

/// Thread-safe shared reference to a cache backend.
pub type SharedCache = Arc<dyn CacheBackend>;

/// Object-safe cache backend trait.
///
/// Keys are arbitrary strings. Values are opaque byte slices — callers handle
/// serialization. All methods are synchronous (called from `spawn_blocking`).
pub trait CacheBackend: Send + Sync {
    /// Retrieve a cached value. Returns `None` on cache miss.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails (e.g. network/IO error for remote caches).
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;

    /// Store a value. Overwrites any existing entry for the key.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails.
    fn set(&self, key: &str, value: &[u8]) -> Result<()>;

    /// Remove a single key. No error if the key doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails.
    fn delete(&self, key: &str) -> Result<()>;

    /// Remove all entries from the cache.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails.
    fn clear(&self) -> Result<()>;

    /// Check whether a key exists without retrieving its value.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails.
    fn has(&self, key: &str) -> Result<bool>;

    /// Return the backend identifier (`"memory"`, `"redis"`, `"none"`, `"custom"`).
    fn kind(&self) -> &'static str;
}
