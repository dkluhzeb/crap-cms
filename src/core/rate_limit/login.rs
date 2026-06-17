//! Per-key login rate limiter with sliding window.

use std::sync::Arc;

use tracing::{error, warn};

use super::{MemoryRateLimitBackend, SharedRateLimitBackend};

/// Per-key login rate limiter. Tracks failed attempts in a sliding window
/// and blocks further attempts after a configurable threshold.
///
/// Thread-safe — the backend handles synchronization internally.
pub struct LoginRateLimiter {
    backend: SharedRateLimitBackend,
    prefix: String,
    max_attempts: u32,
    window_secs: u64,
}

impl LoginRateLimiter {
    /// Create a rate limiter with an explicit backend and prefix.
    ///
    /// `prefix` distinguishes different limiters sharing the same backend
    /// (e.g., `"login"`, `"ip_login"`, `"forgot"`, `"ip_forgot"`).
    pub fn with_backend(
        backend: SharedRateLimitBackend,
        prefix: &str,
        max_attempts: u32,
        window_seconds: u64,
    ) -> Self {
        Self {
            backend,
            prefix: prefix.to_string(),
            max_attempts,
            window_secs: window_seconds,
        }
    }

    /// Create a rate limiter with the default in-memory backend.
    ///
    /// Convenience constructor for single-server deployments and tests.
    #[must_use]
    pub fn new(max_attempts: u32, window_seconds: u64) -> Self {
        Self::with_backend(
            Arc::new(MemoryRateLimitBackend::new()),
            "",
            max_attempts,
            window_seconds,
        )
    }

    /// Build the prefixed key for backend storage.
    fn prefixed_key(&self, key: &str) -> String {
        format!("{}:{}", self.prefix, key)
    }

    /// Check if a key is currently blocked (too many recent failures).
    ///
    /// Fails CLOSED: when the backend can't be reached (e.g. a Redis outage),
    /// the key is treated as blocked. Failing open here would silently
    /// disable brute-force protection for exactly as long as the outage —
    /// an attacker-friendly window. The error is logged so operators can
    /// tell infrastructure trouble from real lockouts.
    #[must_use]
    pub fn is_blocked(&self, key: &str) -> bool {
        let pkey = self.prefixed_key(key);

        match self.backend.count(&pkey, self.window_secs) {
            Ok(count) => count >= self.max_attempts,
            Err(e) => {
                error!("Rate limit backend unavailable — failing closed: {e:#}");
                true
            }
        }
    }

    /// Atomically record an attempt and report whether it is BLOCKED.
    ///
    /// Performs the limit check and the increment as a single backend
    /// operation, closing the check-then-record race that `is_blocked` +
    /// `record_failure` leaves open: under that split, a burst of concurrent
    /// requests can all observe an under-limit count before any of them
    /// records, letting more than `max_attempts` through per window. Call this
    /// once at the start of an attempt instead.
    ///
    /// Returns `true` when the key is at/over the limit (the attempt should be
    /// rejected); the at-limit call does not increment further, so the count
    /// stays bounded. On a successful operation, call [`Self::clear`] to wipe
    /// the recorded attempts.
    ///
    /// Fails CLOSED on backend error (treats the attempt as blocked), matching
    /// [`Self::is_blocked`].
    #[must_use]
    pub fn check_and_block(&self, key: &str) -> bool {
        let pkey = self.prefixed_key(key);

        match self
            .backend
            .check_and_record(&pkey, self.max_attempts, self.window_secs)
        {
            Ok(allowed) => !allowed,
            Err(e) => {
                error!("Rate limit backend unavailable — failing closed: {e:#}");
                true
            }
        }
    }

    /// Record a failed attempt for the given key.
    pub fn record_failure(&self, key: &str) {
        let pkey = self.prefixed_key(key);

        if let Err(e) = self.backend.record(&pkey, self.window_secs) {
            warn!("Rate limit record failed: {:#}", e);
        }
    }

    /// Clear all failed attempts for the given key (e.g., on successful login).
    pub fn clear(&self, key: &str) {
        let pkey = self.prefixed_key(key);

        if let Err(e) = self.backend.clear(&pkey) {
            warn!("Rate limit clear failed: {:#}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_backend() -> SharedRateLimitBackend {
        Arc::new(MemoryRateLimitBackend::new())
    }

    #[test]
    fn is_blocked_initially_false() {
        let limiter = LoginRateLimiter::with_backend(memory_backend(), "test", 3, 60);
        assert!(!limiter.is_blocked("test@example.com"));
    }

    #[test]
    fn blocks_after_max_attempts() {
        let limiter = LoginRateLimiter::with_backend(memory_backend(), "test", 3, 60);
        limiter.record_failure("test@example.com");
        limiter.record_failure("test@example.com");
        assert!(!limiter.is_blocked("test@example.com"));
        limiter.record_failure("test@example.com");
        assert!(limiter.is_blocked("test@example.com"));
    }

    #[test]
    fn clear_resets_attempts() {
        let limiter = LoginRateLimiter::with_backend(memory_backend(), "test", 2, 60);
        limiter.record_failure("a@b.com");
        limiter.record_failure("a@b.com");
        assert!(limiter.is_blocked("a@b.com"));
        limiter.clear("a@b.com");
        assert!(!limiter.is_blocked("a@b.com"));
    }

    #[test]
    fn different_emails_independent() {
        let limiter = LoginRateLimiter::with_backend(memory_backend(), "test", 2, 60);
        limiter.record_failure("a@b.com");
        limiter.record_failure("a@b.com");
        assert!(limiter.is_blocked("a@b.com"));
        assert!(!limiter.is_blocked("c@d.com"));
    }

    #[test]
    fn expired_attempts_are_pruned() {
        let limiter = LoginRateLimiter::with_backend(memory_backend(), "test", 2, 0);
        limiter.record_failure("a@b.com");
        limiter.record_failure("a@b.com");
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(!limiter.is_blocked("a@b.com"));
    }

    #[test]
    fn prefix_isolates_limiters() {
        let backend = memory_backend();
        let login = LoginRateLimiter::with_backend(backend.clone(), "login", 2, 60);
        let forgot = LoginRateLimiter::with_backend(backend, "forgot", 2, 60);

        login.record_failure("a@b.com");
        login.record_failure("a@b.com");
        assert!(login.is_blocked("a@b.com"));
        assert!(!forgot.is_blocked("a@b.com"));
    }

    #[test]
    fn check_and_block_records_and_blocks_at_limit() {
        let limiter = LoginRateLimiter::with_backend(memory_backend(), "test", 3, 60);

        // Each call records; under the limit it does not block.
        assert!(!limiter.check_and_block("a@b.com"));
        assert!(!limiter.check_and_block("a@b.com"));
        assert!(!limiter.check_and_block("a@b.com"));

        // At the limit: blocked, and the count stays bounded (no over-count).
        assert!(limiter.check_and_block("a@b.com"));
        assert!(limiter.check_and_block("a@b.com"));

        // A success clears, re-opening the window.
        limiter.clear("a@b.com");
        assert!(!limiter.check_and_block("a@b.com"));
    }

    /// Security: a backend failure must fail CLOSED (blocked), not silently
    /// disable brute-force protection for the duration of the outage.
    #[test]
    fn check_and_block_fails_closed_on_backend_error() {
        struct FailingBackend;
        impl super::super::RateLimitBackend for FailingBackend {
            fn count(&self, _key: &str, _window_secs: u64) -> anyhow::Result<u32> {
                anyhow::bail!("backend down")
            }
            fn record(&self, _key: &str, _window_secs: u64) -> anyhow::Result<()> {
                anyhow::bail!("backend down")
            }
            fn clear(&self, _key: &str) -> anyhow::Result<()> {
                anyhow::bail!("backend down")
            }
            fn check_and_record(
                &self,
                _key: &str,
                _max_count: u32,
                _window_secs: u64,
            ) -> anyhow::Result<bool> {
                anyhow::bail!("backend down")
            }
            fn kind(&self) -> &'static str {
                "failing"
            }
        }

        let limiter = LoginRateLimiter::with_backend(Arc::new(FailingBackend), "t", 5, 60);
        assert!(
            limiter.check_and_block("user@example.com"),
            "backend failure must block (fail closed)"
        );
    }

    /// Security: a backend failure must fail CLOSED (blocked), not silently
    /// disable brute-force protection for the duration of the outage.
    #[test]
    fn backend_error_fails_closed() {
        struct FailingBackend;
        impl super::super::RateLimitBackend for FailingBackend {
            fn count(&self, _key: &str, _window_secs: u64) -> anyhow::Result<u32> {
                anyhow::bail!("backend down")
            }
            fn record(&self, _key: &str, _window_secs: u64) -> anyhow::Result<()> {
                anyhow::bail!("backend down")
            }
            fn clear(&self, _key: &str) -> anyhow::Result<()> {
                anyhow::bail!("backend down")
            }
            fn check_and_record(
                &self,
                _key: &str,
                _max_count: u32,
                _window_secs: u64,
            ) -> anyhow::Result<bool> {
                anyhow::bail!("backend down")
            }
            fn kind(&self) -> &'static str {
                "failing"
            }
        }

        let limiter = LoginRateLimiter::with_backend(Arc::new(FailingBackend), "t", 5, 60);
        assert!(
            limiter.is_blocked("user@example.com"),
            "backend failure must block (fail closed)"
        );
    }
}
