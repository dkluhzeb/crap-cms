//! Cache backend abstraction for cross-request caching.
//!
//! Provides a trait-based backend system: `memory` (default DashMap),
//! `redis` (feature-flagged), `none` (no-op), and `custom` (Lua-delegated).

mod custom;
mod factory;
mod memory;
mod none;
#[cfg(feature = "redis")]
mod redis;

use std::sync::Arc;

use anyhow::Result;

pub use custom::CustomCache;
pub use factory::create_cache;
pub use memory::MemoryCache;
pub use none::NoneCache;

/// Thread-safe shared reference to a cache backend.
pub type SharedCache = Arc<dyn CacheBackend>;

/// Object-safe cache backend trait.
///
/// Keys are arbitrary strings. Values are opaque byte slices — callers handle
/// serialization. All methods are synchronous (called from `spawn_blocking`).
pub trait CacheBackend: Send + Sync {
    /// Retrieve a cached value. Returns `None` on cache miss.
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;

    /// Store a value. Overwrites any existing entry for the key.
    fn set(&self, key: &str, value: &[u8]) -> Result<()>;

    /// Remove a single key. No error if the key doesn't exist.
    fn delete(&self, key: &str) -> Result<()>;

    /// Remove all entries from the cache.
    fn clear(&self) -> Result<()>;

    /// Check whether a key exists without retrieving its value.
    fn has(&self, key: &str) -> Result<bool>;

    /// Return the backend identifier (`"memory"`, `"redis"`, `"none"`, `"custom"`).
    fn kind(&self) -> &'static str;
}
