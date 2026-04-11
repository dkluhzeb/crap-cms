//! Per-IP gRPC rate limiter with sliding window.

use std::sync::Arc;

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

    #[test]
    fn window_expiry_resets() {
        let limiter = GrpcRateLimiter::with_backend(memory_backend(), 2, 0);
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("1.2.3.4"));
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert!(limiter.check_and_record("1.2.3.4"));
    }
}
