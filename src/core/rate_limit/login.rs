//! Per-key login rate limiter with sliding window.

use std::sync::Arc;

use tracing::warn;

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
    pub fn is_blocked(&self, key: &str) -> bool {
        let pkey = self.prefixed_key(key);

        self.backend
            .count(&pkey, self.window_secs)
            .map(|count| count >= self.max_attempts)
            .unwrap_or(false)
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
}
