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

    /// Remove the single most-recent event for `key`, if any ("refund" one
    /// increment). Used to undo an attempt that turned out legitimate (e.g. a
    /// successful login) so it doesn't count toward a shared per-IP limit —
    /// without wiping the other, still-suspect events the way [`clear`] would.
    ///
    /// The default is a safe no-op: leaving the event counted only errs toward
    /// blocking, never toward permitting more attempts. Backends that can
    /// cheaply drop the newest event override this.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend fails.
    fn refund(&self, key: &str, window_secs: u64) -> Result<()> {
        let _ = (key, window_secs);
        Ok(())
    }

    /// Backend identifier (`"memory"`, `"redis"`, `"none"`).
    fn kind(&self) -> &'static str;
}
