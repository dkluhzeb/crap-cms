//! In-memory login rate limiter: tracks failed attempts per email
//! and blocks further attempts after a configurable threshold.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

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
        let mut map = match self.attempts.lock() {
            Ok(m) => m,
            Err(_) => return false,
        };
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
        let mut map = match self.attempts.lock() {
            Ok(m) => m,
            Err(_) => return,
        };
        let now = Instant::now();
        let times = map.entry(email.to_string()).or_default();
        times.retain(|t| now.duration_since(*t) < self.window);
        times.push(now);
    }

    /// Clear all failed attempts for the given email (e.g., on successful login).
    pub fn clear(&self, email: &str) {
        let mut map = match self.attempts.lock() {
            Ok(m) => m,
            Err(_) => return,
        };
        map.remove(email);
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
}
