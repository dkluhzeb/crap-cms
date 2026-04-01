//! Rate limiting with pluggable backends.
//!
//! Provides `LoginRateLimiter` (sliding-window per-key blocking) and
//! `GrpcRateLimiter` (sliding-window per-IP request limiting), both
//! backed by a `RateLimitBackend` trait.
//!
//! Backends: `memory` (default), `redis` (feature-flagged), `none` (disabled).

mod memory;
mod none;
#[cfg(feature = "redis")]
mod redis;

use std::sync::Arc;

use anyhow::Result;
use tracing::info;

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
    ///
    /// This MUST be atomic — no other operation on the same key can
    /// interleave between the count check and the record. For multi-server
    /// backends (Redis), this is implemented as a single Lua script.
    fn check_and_record(&self, key: &str, max_count: u32, window_secs: u64) -> Result<bool>;

    /// Remove all events for `key`.
    fn clear(&self, key: &str) -> Result<()>;

    /// Backend identifier (`"memory"`, `"redis"`, `"none"`).
    fn kind(&self) -> &'static str;
}

/// Create the appropriate rate limit backend from config.
pub fn create_rate_limit_backend(
    backend: &str,
    #[allow(unused_variables)] redis_url: &str,
    #[allow(unused_variables)] prefix: &str,
) -> Result<SharedRateLimitBackend> {
    match backend {
        "memory" | "" => {
            info!("Using memory rate limit backend");
            Ok(Arc::new(MemoryRateLimitBackend::new()))
        }
        "none" => {
            info!("Rate limiting disabled (none backend)");
            Ok(Arc::new(NoneRateLimitBackend))
        }
        #[cfg(feature = "redis")]
        "redis" => {
            info!(url = %redis_url, prefix = %prefix, "Using Redis rate limit backend");
            Ok(Arc::new(redis::RedisRateLimitBackend::new(
                redis_url, prefix,
            )?))
        }
        #[cfg(not(feature = "redis"))]
        "redis" => {
            anyhow::bail!(
                "Redis rate limit backend requires the `redis` feature. \
                 Rebuild with `--features redis`."
            );
        }
        other => anyhow::bail!("Unknown rate limit backend: '{}'", other),
    }
}

// ── LoginRateLimiter ─────────────────────────────────────────────────────

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
            tracing::warn!("Rate limit record failed: {:#}", e);
        }
    }

    /// Clear all failed attempts for the given key (e.g., on successful login).
    pub fn clear(&self, key: &str) {
        let pkey = self.prefixed_key(key);

        if let Err(e) = self.backend.clear(&pkey) {
            tracing::warn!("Rate limit clear failed: {:#}", e);
        }
    }
}

// ── GrpcRateLimiter ──────────────────────────────────────────────────────

/// Per-IP gRPC rate limiter. Sliding-window counter per IP address.
/// When `max_requests == 0`, rate limiting is disabled (all requests pass).
pub struct GrpcRateLimiter {
    backend: SharedRateLimitBackend,
    max_requests: u32,
    window_secs: u64,
}

impl GrpcRateLimiter {
    /// Create a rate limiter with an explicit backend.
    pub fn with_backend(
        backend: SharedRateLimitBackend,
        max_requests: u32,
        window_seconds: u64,
    ) -> Self {
        Self {
            backend,
            max_requests,
            window_secs: window_seconds,
        }
    }

    /// Create a rate limiter with the default in-memory backend.
    pub fn new(max_requests: u32, window_seconds: u64) -> Self {
        Self::with_backend(
            Arc::new(MemoryRateLimitBackend::new()),
            max_requests,
            window_seconds,
        )
    }

    /// Check if a request from `ip` is allowed and record it atomically.
    /// Returns `true` if the request is within the limit (or limiting is disabled).
    pub fn check_and_record(&self, ip: &str) -> bool {
        if self.max_requests == 0 {
            return true;
        }

        let key = format!("grpc:{}", ip);

        self.backend
            .check_and_record(&key, self.max_requests, self.window_secs)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_backend() -> SharedRateLimitBackend {
        Arc::new(MemoryRateLimitBackend::new())
    }

    // ── LoginRateLimiter tests ───────────────────────────────────────────

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

    // ── GrpcRateLimiter tests ────────────────────────────────────────────

    #[test]
    fn grpc_disabled_allows_all() {
        let limiter = GrpcRateLimiter::with_backend(memory_backend(), 0, 60);
        for _ in 0..1000 {
            assert!(limiter.check_and_record("1.2.3.4"));
        }
    }

    #[test]
    fn grpc_blocks_at_limit() {
        let limiter = GrpcRateLimiter::with_backend(memory_backend(), 3, 60);
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(!limiter.check_and_record("1.2.3.4"));
    }

    #[test]
    fn grpc_different_ips_independent() {
        let limiter = GrpcRateLimiter::with_backend(memory_backend(), 2, 60);
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(!limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("5.6.7.8"));
    }

    #[test]
    fn grpc_window_expiry_resets() {
        let limiter = GrpcRateLimiter::with_backend(memory_backend(), 2, 0);
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("1.2.3.4"));
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(limiter.check_and_record("1.2.3.4"));
    }

    // ── NoneRateLimitBackend tests ───────────────────────────────────────

    #[test]
    fn none_backend_never_blocks() {
        let backend: SharedRateLimitBackend = Arc::new(NoneRateLimitBackend);
        let limiter = LoginRateLimiter::with_backend(backend, "test", 1, 60);
        for _ in 0..100 {
            limiter.record_failure("a@b.com");
        }
        assert!(!limiter.is_blocked("a@b.com"));
    }

    // ── Factory tests ────────────────────────────────────────────────────

    #[test]
    fn create_memory_backend() {
        let backend = create_rate_limit_backend("memory", "", "").unwrap();
        assert_eq!(backend.kind(), "memory");
    }

    #[test]
    fn create_none_backend() {
        let backend = create_rate_limit_backend("none", "", "").unwrap();
        assert_eq!(backend.kind(), "none");
    }

    #[test]
    fn create_unknown_backend_errors() {
        let result = create_rate_limit_backend("memcached", "", "");
        assert!(result.is_err());
    }
}
