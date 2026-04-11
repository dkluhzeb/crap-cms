//! Rate limiting with pluggable backends.
//!
//! Provides `LoginRateLimiter` (sliding-window per-key blocking) and
//! `GrpcRateLimiter` (sliding-window per-IP request limiting), both
//! backed by a `RateLimitBackend` trait.
//!
//! Backends: `memory` (default), `redis` (feature-flagged), `none` (disabled).

mod factory;
mod grpc;
mod login;
mod memory;
mod none;
#[cfg(feature = "redis")]
mod redis;

use std::sync::Arc;

use anyhow::Result;

pub use factory::create_rate_limit_backend;
pub use grpc::GrpcRateLimiter;
pub use login::LoginRateLimiter;
pub use memory::MemoryRateLimitBackend;
pub use none::NoneRateLimitBackend;

/// Thread-safe shared reference to a rate limit backend.
pub type SharedRateLimitBackend = Arc<dyn RateLimitBackend>;

/// Object-safe rate limit storage backend.
///
/// Stores timestamped events keyed by string. The backend handles storage
/// and expiry; callers apply threshold logic via wrapper structs.
pub trait RateLimitBackend: Send + Sync {
    /// Count events for `key` within the last `window_secs`.
    fn count(&self, key: &str, window_secs: u64) -> Result<u32>;

    /// Record an event for `key`. `window_secs` is a hint for expiry/eviction.
    fn record(&self, key: &str, window_secs: u64) -> Result<()>;

    /// Atomically check if under `max_count` and record if so.
    ///
    /// Returns `true` if the event was recorded (under limit),
    /// `false` if rate-limited (at or over `max_count`).
    fn check_and_record(&self, key: &str, max_count: u32, window_secs: u64) -> Result<bool>;

    /// Remove all events for `key`.
    fn clear(&self, key: &str) -> Result<()>;

    /// Backend identifier (`"memory"`, `"redis"`, `"none"`).
    fn kind(&self) -> &'static str;
}
