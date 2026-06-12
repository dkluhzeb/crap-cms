//! Per-IP gRPC rate limiter with sliding window.

use std::sync::Arc;

use tracing::error;

use super::{MemoryRateLimitBackend, SharedRateLimitBackend};

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
    #[must_use]
    pub fn new(max_requests: u32, window_seconds: u64) -> Self {
        Self::with_backend(
            Arc::new(MemoryRateLimitBackend::new()),
            max_requests,
            window_seconds,
        )
    }

    /// Check if a request from `ip` is allowed and record it atomically.
    /// Returns `true` if the request is within the limit (or limiting is disabled).
    #[must_use]
    pub fn check_and_record(&self, ip: &str) -> bool {
        if self.max_requests == 0 {
            return true;
        }

        let key = format!("grpc:{ip}");

        self.backend
            .check_and_record(&key, self.max_requests, self.window_secs)
            .inspect_err(|e| error!("Rate limit backend unavailable — failing closed: {e:#}"))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_backend() -> SharedRateLimitBackend {
        Arc::new(MemoryRateLimitBackend::new())
    }

    #[test]
    fn disabled_allows_all() {
        let limiter = GrpcRateLimiter::with_backend(memory_backend(), 0, 60);
        for _ in 0..1000 {
            assert!(limiter.check_and_record("1.2.3.4"));
        }
    }

    #[test]
    fn blocks_at_limit() {
        let limiter = GrpcRateLimiter::with_backend(memory_backend(), 3, 60);
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(!limiter.check_and_record("1.2.3.4"));
    }

    #[test]
    fn different_ips_independent() {
        let limiter = GrpcRateLimiter::with_backend(memory_backend(), 2, 60);
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(!limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("5.6.7.8"));
    }

    /// Security: a backend failure must fail CLOSED (denied), not silently
    /// disable per-IP limiting for the duration of the outage.
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

        let limiter = GrpcRateLimiter::with_backend(Arc::new(FailingBackend), 5, 60);
        assert!(
            !limiter.check_and_record("1.2.3.4"),
            "backend failure must deny (fail closed)"
        );
    }

    #[test]
    fn window_expiry_resets() {
        let limiter = GrpcRateLimiter::with_backend(memory_backend(), 2, 0);
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("1.2.3.4"));
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(limiter.check_and_record("1.2.3.4"));
    }
}
