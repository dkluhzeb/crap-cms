//! In-memory rate limit backend using `HashMap` with sliding window.

use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use anyhow::Result;

use super::RateLimitBackend;

/// Number of unique keys at which the first sweep of expired entries runs.
const MAX_MAP_SIZE: usize = 100_000;

/// One key's recorded events, with the window they are counted under.
///
/// The window is stored per key because limiters with different windows share
/// one backend. A sweep triggered by one limiter must prune every key by that
/// key's OWN window — pruning by the caller's would delete another limiter's
/// still-live lockouts whenever its window is shorter.
struct KeyEvents {
    window: Duration,
    times: Vec<Instant>,
}

impl KeyEvents {
    fn prune(&mut self, now: Instant) {
        let window = self.window;
        self.times.retain(|t| now.duration_since(*t) < window);
    }
}

struct State {
    events: HashMap<String, KeyEvents>,
    /// Key count at which the next sweep runs: twice the keys left by the last
    /// sweep, never below `sweep_floor`. A map full of live keys is not
    /// rescanned on every call, and the threshold drops back once a spike of
    /// keys has expired.
    sweep_at: usize,
    sweep_floor: usize,
}

/// In-memory rate limit backend. Stores timestamped events per key
/// in a `HashMap` behind a `Mutex`.
pub struct MemoryRateLimitBackend {
    state: Mutex<State>,
}

impl Default for MemoryRateLimitBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryRateLimitBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::with_sweep_threshold(MAX_MAP_SIZE)
    }

    fn with_sweep_threshold(threshold: usize) -> Self {
        Self {
            state: Mutex::new(State {
                events: HashMap::new(),
                sweep_at: threshold,
                sweep_floor: threshold,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Fetch `key`'s events for a write under `window_secs`, sweeping expired
    /// keys first when the map has grown past the threshold.
    fn entry<'a>(
        state: &'a mut State,
        key: &str,
        window_secs: u64,
        now: Instant,
    ) -> &'a mut KeyEvents {
        if state.events.len() >= state.sweep_at {
            state.events.retain(|_, events| {
                events.prune(now);
                !events.times.is_empty()
            });
            state.sweep_at = state.sweep_floor.max(state.events.len().saturating_mul(2));
        }

        let window = Duration::from_secs(window_secs);
        let events = state
            .events
            .entry(key.to_string())
            .or_insert_with(|| KeyEvents {
                window,
                times: Vec::new(),
            });
        events.window = window;
        events.prune(now);

        events
    }
}

/// Saturate at `u32::MAX` so a runaway event log still rate-limits.
fn saturating_len(times: &[Instant]) -> u32 {
    u32::try_from(times.len()).unwrap_or(u32::MAX)
}

impl RateLimitBackend for MemoryRateLimitBackend {
    fn count(&self, key: &str, window_secs: u64) -> Result<u32> {
        let mut state = self.lock();
        let window = Duration::from_secs(window_secs);
        let now = Instant::now();

        let Some(events) = state.events.get_mut(key) else {
            return Ok(0);
        };
        events.times.retain(|t| now.duration_since(*t) < window);

        Ok(saturating_len(&events.times))
    }

    fn record(&self, key: &str, window_secs: u64) -> Result<()> {
        let mut state = self.lock();
        let now = Instant::now();

        Self::entry(&mut state, key, window_secs, now)
            .times
            .push(now);

        Ok(())
    }

    fn check_and_record(&self, key: &str, max_count: u32, window_secs: u64) -> Result<bool> {
        let mut state = self.lock();
        let now = Instant::now();

        let events = Self::entry(&mut state, key, window_secs, now);
        if saturating_len(&events.times) >= max_count {
            return Ok(false);
        }

        events.times.push(now);

        Ok(true)
    }

    fn clear(&self, key: &str) -> Result<()> {
        self.lock().events.remove(key);

        Ok(())
    }

    fn refund(&self, key: &str, window_secs: u64) -> Result<()> {
        let mut state = self.lock();
        let window = Duration::from_secs(window_secs);
        let now = Instant::now();

        let Some(events) = state.events.get_mut(key) else {
            return Ok(());
        };
        events.times.retain(|t| now.duration_since(*t) < window);
        // Events are pushed in time order, so the last is the most recent.
        events.times.pop();

        if events.times.is_empty() {
            state.events.remove(key);
        }

        Ok(())
    }

    fn kind(&self) -> &'static str {
        "memory"
    }
}

#[cfg(test)]
mod tests {
    use std::thread::sleep;

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
        sleep(Duration::from_millis(10));
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
        sleep(Duration::from_millis(10));
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

    /// A sweep triggered by a short-window limiter must not delete a
    /// long-window limiter's live lockout: each key prunes by its own window.
    #[test]
    fn sweep_prunes_each_key_by_its_own_window() {
        let backend = MemoryRateLimitBackend::with_sweep_threshold(2);

        backend.record("lockout", 3600).unwrap();
        backend.record("burst-a", 0).unwrap();
        sleep(Duration::from_millis(5));

        // The map is at the threshold, so this write sweeps.
        backend.record("burst-b", 0).unwrap();

        assert_eq!(backend.count("lockout", 3600).unwrap(), 1);
        assert!(!backend.lock().events.contains_key("burst-a"));
    }

    /// A sweep that frees nothing raises the threshold, so a map of live keys
    /// is not rescanned on every write.
    #[test]
    fn sweep_threshold_grows_when_keys_stay_live() {
        let backend = MemoryRateLimitBackend::with_sweep_threshold(2);

        backend.record("a", 3600).unwrap();
        backend.record("b", 3600).unwrap();
        backend.record("c", 3600).unwrap();

        assert!(backend.lock().sweep_at >= 4);
    }

    /// Once a spike of keys has expired, the threshold returns to its floor
    /// instead of staying at twice the peak.
    #[test]
    fn sweep_threshold_returns_to_its_floor_after_a_spike() {
        let backend = MemoryRateLimitBackend::with_sweep_threshold(2);

        for key in ["a", "b", "c", "d"] {
            backend.record(key, 0).unwrap();
        }
        sleep(Duration::from_millis(5));
        backend.record("e", 3600).unwrap();

        assert!(backend.lock().sweep_at <= 2);
    }
}
