//! Per-IP gRPC rate limiter with sliding window.

use std::sync::Arc;

use tracing::error;

use super::{MemoryRateLimitBackend, SharedRateLimitBackend};
use crate::core::ClientIp;

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

    /// Check if a request from `client` is allowed and record it atomically.
    /// Returns `true` if the request is within the limit (or limiting is disabled).
    ///
    /// Counts against the client's rate-limit bucket
    /// ([`ClientIp::rate_limit_key`]), so an IPv6 client shares one budget
    /// across its /64.
    #[must_use]
    pub fn check_and_record(&self, client: &ClientIp) -> bool {
        if self.max_requests == 0 {
            return true;
        }

        let key = format!("grpc:{}", client.rate_limit_key());

        self.backend
            .check_and_record(&key, self.max_requests, self.window_secs)
            .inspect_err(|e| error!("Rate limit backend unavailable — failing closed: {e:#}"))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use std::{thread::sleep, time::Duration};

    use anyhow::{Result as AnyResult, bail};

    use super::*;
    use crate::core::rate_limit::RateLimitBackend;

    fn client(ip: &str) -> ClientIp {
        ClientIp::new(ip.parse().unwrap())
    }

    fn memory_backend() -> SharedRateLimitBackend {
        Arc::new(MemoryRateLimitBackend::new())
    }

    #[test]
    fn disabled_allows_all() {
        let limiter = GrpcRateLimiter::with_backend(memory_backend(), 0, 60);
        for _ in 0..1000 {
            assert!(limiter.check_and_record(&client("1.2.3.4")));
        }
    }

    #[test]
    fn blocks_at_limit() {
        let limiter = GrpcRateLimiter::with_backend(memory_backend(), 3, 60);
        assert!(limiter.check_and_record(&client("1.2.3.4")));
        assert!(limiter.check_and_record(&client("1.2.3.4")));
        assert!(limiter.check_and_record(&client("1.2.3.4")));
        assert!(!limiter.check_and_record(&client("1.2.3.4")));
    }

    #[test]
    fn different_ips_independent() {
        let limiter = GrpcRateLimiter::with_backend(memory_backend(), 2, 60);
        assert!(limiter.check_and_record(&client("1.2.3.4")));
        assert!(limiter.check_and_record(&client("1.2.3.4")));
        assert!(!limiter.check_and_record(&client("1.2.3.4")));
        assert!(limiter.check_and_record(&client("5.6.7.8")));
    }

    /// Security: a backend failure must fail CLOSED (denied), not silently
    /// disable per-IP limiting for the duration of the outage.
    #[test]
    fn backend_error_fails_closed() {
        struct FailingBackend;
        impl RateLimitBackend for FailingBackend {
            fn count(&self, _key: &str, _window_secs: u64) -> AnyResult<u32> {
                bail!("backend down")
            }
            fn record(&self, _key: &str, _window_secs: u64) -> AnyResult<()> {
                bail!("backend down")
            }
            fn clear(&self, _key: &str) -> AnyResult<()> {
                bail!("backend down")
            }
            fn check_and_record(
                &self,
                _key: &str,
                _max_count: u32,
                _window_secs: u64,
            ) -> AnyResult<bool> {
                bail!("backend down")
            }
            fn kind(&self) -> &'static str {
                "failing"
            }
        }

        let limiter = GrpcRateLimiter::with_backend(Arc::new(FailingBackend), 5, 60);
        assert!(
            !limiter.check_and_record(&client("1.2.3.4")),
            "backend failure must deny (fail closed)"
        );
    }

    /// Regression: the limiter keyed IPv6 per /128, so a client rotating
    /// addresses inside its /64 got a fresh budget per request.
    #[test]
    fn ipv6_clients_share_a_budget_across_their_slash_64() {
        let limiter = GrpcRateLimiter::with_backend(memory_backend(), 1, 60);

        assert!(limiter.check_and_record(&client("2001:db8:1:2::1")));
        assert!(!limiter.check_and_record(&client("2001:db8:1:2::2")));
        assert!(limiter.check_and_record(&client("2001:db8:1:3::1")));
    }

    #[test]
    fn window_expiry_resets() {
        let limiter = GrpcRateLimiter::with_backend(memory_backend(), 2, 0);
        assert!(limiter.check_and_record(&client("1.2.3.4")));
        assert!(limiter.check_and_record(&client("1.2.3.4")));
        sleep(Duration::from_millis(10));
        assert!(limiter.check_and_record(&client("1.2.3.4")));
    }
}
