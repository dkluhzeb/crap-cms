//! Cache backend abstraction for cross-request caching.
//!
//! Provides a trait-based backend system: `memory` (default `DashMap`),
//! `redis` (feature-flagged), `none` (no-op), and `custom` (Lua-delegated).
//!
//! The [`CacheBackend`] trait + [`SharedCache`] type alias live in the
//! sibling [`backend`] module; sub-modules (`memory`, `redis`, `none`,
//! `custom`) implement the trait. The factory ([`create_cache`]) picks
//! the implementation by config.

mod backend;
mod custom;
mod factory;
mod keys;
mod memory;
mod none;
mod periodic;
#[cfg(feature = "redis")]
mod redis;

pub use backend::{CacheBackend, SharedCache};
pub use custom::CustomCache;
pub use factory::{create_cache, create_cache_with_lease, warn_if_custom_cache_multi_vm};
pub use keys::{CACHE_KEY_NAMESPACE, cache_clear_pattern, cache_key, cache_namespace};
pub use memory::MemoryCache;
pub use none::NoneCache;
pub use periodic::{periodic_clear_interval, spawn_periodic_clear};
