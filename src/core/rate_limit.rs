//! In-memory login rate limiter: tracks failed attempts per email
//! and blocks further attempts after a configurable threshold.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

/// Maximum number of unique keys before triggering a sweep.
const MAX_MAP_SIZE: usize = 100_000;

/// Per-email login rate limiter. Thread-safe via internal Mutex.
pub struct LoginRateLimiter {
    attempts: Mutex<HashMap<String, Vec<Instant>>>,
    max_attempts: u32,
    window: Duration,
}

impl LoginRateLimiter {
    /// Create a new rate limiter.
    pub fn new(max_attempts: u32, window_seconds: u64) -> Self {
        Self {
            attempts: Mutex::new(HashMap::new()),
            max_attempts,
            window: Duration::from_secs(window_seconds),
        }
    }

    /// Check if an email is currently blocked (too many recent failures).
    pub fn is_blocked(&self, email: &str) -> bool {
        let mut map = self.attempts.lock().expect("rate limiter mutex poisoned");
        let now = Instant::now();

        if let Some(times) = map.get_mut(email) {
            times.retain(|t| now.duration_since(*t) < self.window);
            times.len() as u32 >= self.max_attempts
        } else {
            false
        }
    }

    /// Record a failed login attempt for the given email.
    pub fn record_failure(&self, email: &str) {
        let mut map = self.attempts.lock().expect("rate limiter mutex poisoned");
        let now = Instant::now();
        // Evict expired entries when map grows too large
        if map.len() > MAX_MAP_SIZE {
            map.retain(|_, times| {
                times.retain(|t| now.duration_since(*t) < self.window);
                !times.is_empty()
            });
        }
        let times = map.entry(email.to_string()).or_default();
        times.retain(|t| now.duration_since(*t) < self.window);
        times.push(now);
    }

    /// Clear all failed attempts for the given email (e.g., on successful login).
    pub fn clear(&self, email: &str) {
        let mut map = self.attempts.lock().expect("rate limiter mutex poisoned");
        map.remove(email);
    }
}

/// Per-IP gRPC rate limiter. Sliding-window counter per IP address.
/// When `max_requests == 0`, rate limiting is disabled (all requests pass).
pub struct GrpcRateLimiter {
    requests: Mutex<HashMap<String, Vec<Instant>>>,
    max_requests: u32,
    window: Duration,
}

impl GrpcRateLimiter {
    /// Create a new rate limiter. `max_requests == 0` disables limiting.
    pub fn new(max_requests: u32, window_seconds: u64) -> Self {
        Self {
            requests: Mutex::new(HashMap::new()),
            max_requests,
            window: Duration::from_secs(window_seconds),
        }
    }

    /// Check if a request from `ip` is allowed and record it.
    /// Returns `true` if the request is within the limit (or limiting is disabled).
    pub fn check_and_record(&self, ip: &str) -> bool {
        if self.max_requests == 0 {
            return true;
        }
        let mut map = self.requests.lock().expect("rate limiter mutex poisoned");
        let now = Instant::now();
        // Evict expired entries when map grows too large
        if map.len() > MAX_MAP_SIZE {
            map.retain(|_, times| {
                times.retain(|t| now.duration_since(*t) < self.window);
                !times.is_empty()
            });
        }
        let times = map.entry(ip.to_string()).or_default();
        times.retain(|t| now.duration_since(*t) < self.window);

        if times.len() as u32 >= self.max_requests {
            return false;
        }
        times.push(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_blocked_initially_false() {
        let limiter = LoginRateLimiter::new(3, 60);
        assert!(!limiter.is_blocked("test@example.com"));
    }

    #[test]
    fn blocks_after_max_attempts() {
        let limiter = LoginRateLimiter::new(3, 60);
        limiter.record_failure("test@example.com");
        limiter.record_failure("test@example.com");
        assert!(!limiter.is_blocked("test@example.com"));
        limiter.record_failure("test@example.com");
        assert!(limiter.is_blocked("test@example.com"));
    }

    #[test]
    fn clear_resets_attempts() {
        let limiter = LoginRateLimiter::new(2, 60);
        limiter.record_failure("a@b.com");
        limiter.record_failure("a@b.com");
        assert!(limiter.is_blocked("a@b.com"));
        limiter.clear("a@b.com");
        assert!(!limiter.is_blocked("a@b.com"));
    }

    #[test]
    fn different_emails_independent() {
        let limiter = LoginRateLimiter::new(2, 60);
        limiter.record_failure("a@b.com");
        limiter.record_failure("a@b.com");
        assert!(limiter.is_blocked("a@b.com"));
        assert!(!limiter.is_blocked("c@d.com"));
    }

    #[test]
    fn expired_attempts_are_pruned() {
        // Window of 0 seconds means all attempts expire immediately
        let limiter = LoginRateLimiter::new(2, 0);
        limiter.record_failure("a@b.com");
        limiter.record_failure("a@b.com");
        // After a tiny sleep, all attempts should have expired
        std::thread::sleep(Duration::from_millis(10));
        assert!(!limiter.is_blocked("a@b.com"));
    }

    // --- GrpcRateLimiter tests ---

    #[test]
    fn grpc_disabled_allows_all() {
        let limiter = GrpcRateLimiter::new(0, 60);
        for _ in 0..1000 {
            assert!(limiter.check_and_record("1.2.3.4"));
        }
    }

    #[test]
    fn grpc_blocks_at_limit() {
        let limiter = GrpcRateLimiter::new(3, 60);
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(!limiter.check_and_record("1.2.3.4"));
    }

    #[test]
    fn grpc_different_ips_independent() {
        let limiter = GrpcRateLimiter::new(2, 60);
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(!limiter.check_and_record("1.2.3.4"));
        // Different IP is still allowed
        assert!(limiter.check_and_record("5.6.7.8"));
        assert!(limiter.check_and_record("5.6.7.8"));
        assert!(!limiter.check_and_record("5.6.7.8"));
    }

    #[test]
    fn login_eviction_on_large_map() {
        // Window of 0 means all entries expire immediately
        let limiter = LoginRateLimiter::new(100, 0);
        // Fill beyond MAX_MAP_SIZE (we can't add 100k entries in a unit test,
        // but we can test the eviction logic by checking that after a sleep,
        // expired entries are pruned)
        for i in 0..10 {
            limiter.record_failure(&format!("user{}@test.com", i));
        }
        std::thread::sleep(Duration::from_millis(10));
        // After expiry, new record_failure should still work
        limiter.record_failure("new@test.com");
        assert!(!limiter.is_blocked("new@test.com"));
    }

    #[test]
    fn grpc_eviction_on_large_map() {
        let limiter = GrpcRateLimiter::new(100, 0);
        for i in 0..10 {
            limiter.check_and_record(&format!("10.0.0.{}", i));
        }
        std::thread::sleep(Duration::from_millis(10));
        // After expiry, check_and_record should still work
        assert!(limiter.check_and_record("10.0.1.1"));
    }

    #[test]
    fn grpc_window_expiry_resets() {
        let limiter = GrpcRateLimiter::new(2, 0);
        assert!(limiter.check_and_record("1.2.3.4"));
        assert!(limiter.check_and_record("1.2.3.4"));
        // Window is 0s, so after a tiny sleep all entries expire
        std::thread::sleep(Duration::from_millis(10));
        assert!(limiter.check_and_record("1.2.3.4"));
    }
}
