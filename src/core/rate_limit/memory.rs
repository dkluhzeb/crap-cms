//! In-memory rate limit backend using HashMap with sliding window.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::Result;

use super::RateLimitBackend;

/// Maximum number of unique keys before triggering a sweep of expired entries.
const MAX_MAP_SIZE: usize = 100_000;

/// In-memory rate limit backend. Stores timestamped events per key
/// in a `HashMap` behind a `Mutex`.
pub struct MemoryRateLimitBackend {
    events: Mutex<HashMap<String, Vec<Instant>>>,
}

impl Default for MemoryRateLimitBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryRateLimitBackend {
    pub fn new() -> Self {
        Self {
            events: Mutex::new(HashMap::new()),
        }
    }
}

impl RateLimitBackend for MemoryRateLimitBackend {
    fn count(&self, key: &str, window_secs: u64) -> Result<u32> {
        let mut map = self.events.lock().expect("rate limiter mutex poisoned");
        let window = Duration::from_secs(window_secs);
        let now = Instant::now();

        if let Some(times) = map.get_mut(key) {
            times.retain(|t| now.duration_since(*t) < window);
            Ok(times.len() as u32)
        } else {
            Ok(0)
        }
    }

    fn record(&self, key: &str, window_secs: u64) -> Result<()> {
        let mut map = self.events.lock().expect("rate limiter mutex poisoned");
        let window = Duration::from_secs(window_secs);
        let now = Instant::now();

        // Evict expired entries when map grows too large
        if map.len() > MAX_MAP_SIZE {
            map.retain(|_, times| {
                times.retain(|t| now.duration_since(*t) < window);
                !times.is_empty()
            });
        }

        let times = map.entry(key.to_string()).or_default();
        times.retain(|t| now.duration_since(*t) < window);
        times.push(now);

        Ok(())
    }

    fn check_and_record(&self, key: &str, max_count: u32, window_secs: u64) -> Result<bool> {
        let mut map = self.events.lock().expect("rate limiter mutex poisoned");
        let window = Duration::from_secs(window_secs);
        let now = Instant::now();

        // Evict expired entries when map grows too large
        if map.len() > MAX_MAP_SIZE {
            map.retain(|_, times| {
                times.retain(|t| now.duration_since(*t) < window);
                !times.is_empty()
            });
        }

        let times = map.entry(key.to_string()).or_default();
        times.retain(|t| now.duration_since(*t) < window);

        if times.len() as u32 >= max_count {
            return Ok(false);
        }

        times.push(now);

        Ok(true)
    }

    fn clear(&self, key: &str) -> Result<()> {
        let mut map = self.events.lock().expect("rate limiter mutex poisoned");
        map.remove(key);
        Ok(())
    }

    fn kind(&self) -> &'static str {
        "memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_empty_is_zero() {
        let backend = MemoryRateLimitBackend::new();
        assert_eq!(backend.count("k1", 60).unwrap(), 0);
    }

    #[test]
    fn record_increments_count() {
        let backend = MemoryRateLimitBackend::new();
        backend.record("k1", 60).unwrap();
        backend.record("k1", 60).unwrap();
        assert_eq!(backend.count("k1", 60).unwrap(), 2);
    }

    #[test]
    fn clear_resets_count() {
        let backend = MemoryRateLimitBackend::new();
        backend.record("k1", 60).unwrap();
        backend.record("k1", 60).unwrap();
        backend.clear("k1").unwrap();
        assert_eq!(backend.count("k1", 60).unwrap(), 0);
    }

    #[test]
    fn expired_events_pruned() {
        let backend = MemoryRateLimitBackend::new();
        backend.record("k1", 0).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(backend.count("k1", 0).unwrap(), 0);
    }

    #[test]
    fn check_and_record_atomic() {
        let backend = MemoryRateLimitBackend::new();

        assert!(backend.check_and_record("k1", 3, 60).unwrap());
        assert!(backend.check_and_record("k1", 3, 60).unwrap());
        assert!(backend.check_and_record("k1", 3, 60).unwrap());
        // At limit — should be rejected
        assert!(!backend.check_and_record("k1", 3, 60).unwrap());
        // Count should be exactly 3 (not 4)
        assert_eq!(backend.count("k1", 60).unwrap(), 3);
    }

    #[test]
    fn check_and_record_window_expiry() {
        let backend = MemoryRateLimitBackend::new();
        assert!(backend.check_and_record("k1", 1, 0).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(10));
        // Window expired — should allow again
        assert!(backend.check_and_record("k1", 1, 0).unwrap());
    }

    #[test]
    fn different_keys_independent() {
        let backend = MemoryRateLimitBackend::new();
        backend.record("k1", 60).unwrap();
        assert_eq!(backend.count("k1", 60).unwrap(), 1);
        assert_eq!(backend.count("k2", 60).unwrap(), 0);
    }
}
