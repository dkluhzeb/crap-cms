//! `RateLimitBackend` trait + `SharedRateLimitBackend` type alias —
//! the abstraction every rate-limit storage backend in this module
//! satisfies.

use std::sync::Arc;

use anyhow::Result;

/// Thread-safe shared reference to a rate limit backend.
pub type SharedRateLimitBackend = Arc<dyn RateLimitBackend>;

/// Object-safe rate limit storage backend.
///
/// Stores timestamped events keyed by string. The backend handles storage
/// and expiry; callers apply threshold logic via wrapper structs.
pub trait RateLimitBackend: Send + Sync {
    /// Count events for `key` within the last `window_secs`.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails (e.g. network error for remote backends).
    fn count(&self, key: &str, window_secs: u64) -> Result<u32>;

    /// Record an event for `key`. `window_secs` is a hint for expiry/eviction.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails.
    fn record(&self, key: &str, window_secs: u64) -> Result<()>;

    /// Atomically check if under `max_count` and record if so.
    ///
    /// Returns `true` if the event was recorded (under limit),
    /// `false` if rate-limited (at or over `max_count`).
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails.
    fn check_and_record(&self, key: &str, max_count: u32, window_secs: u64) -> Result<bool>;

    /// Remove all events for `key`.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails.
    fn clear(&self, key: &str) -> Result<()>;

    /// Backend identifier (`"memory"`, `"redis"`, `"none"`).
    fn kind(&self) -> &'static str;
}
