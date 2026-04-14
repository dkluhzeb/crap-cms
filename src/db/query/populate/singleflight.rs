//! Singleflight deduplication for concurrent cache-miss fetches.
//!
//! When multiple threads race on the same cache miss, we want exactly one
//! DB fetch to run. The others should block until the first fetch completes
//! and receive the same result. Once the fetch is done, the in-flight slot is
//! removed so the next miss starts a fresh fetch.
//!
//! Poisoning policy: if a fetch closure panics while holding the slot mutex,
//! the mutex is poisoned. We propagate the poison via `.expect(...)` so a
//! panicking fetch fails fast for other waiters on the same key instead of
//! silently retrying. Panics should be rare; if one happens the operator
//! sees the same panic surfaced at every concurrent caller, which is the
//! correct diagnostic signal.

use std::sync::{Arc, Mutex};

use dashmap::DashMap;

/// Deduplicates concurrent cache-miss fetches for the same key.
///
/// When multiple threads race on a cache miss, only the first thread runs
/// the fetch closure; the others block on a shared slot and receive the
/// same result. Once the fetch completes, the slot is removed so future
/// misses start a fresh fetch.
pub struct Singleflight<V: Clone> {
    inflight: DashMap<String, Arc<Mutex<Option<V>>>>,
}

impl<V: Clone> Default for Singleflight<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V: Clone> Singleflight<V> {
    /// Create an empty `Singleflight` with no in-flight entries.
    pub fn new() -> Self {
        Self {
            inflight: DashMap::new(),
        }
    }

    /// Fetch the value for `key`. If another thread is already fetching
    /// the same key, this call blocks until that thread's fetch completes
    /// and returns a clone of the same value.
    pub fn get_or_fetch<F>(&self, key: &str, fetch: F) -> V
    where
        F: FnOnce() -> V,
    {
        // Step 1: find-or-insert the inflight slot, release the dashmap lock quickly.
        let slot = {
            let entry = self
                .inflight
                .entry(key.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(None)));

            entry.value().clone()
        };

        // Step 2: acquire the slot mutex — may block if another thread is fetching.
        // Poison propagates: if the first fetch panicked, subsequent waiters panic
        // too (fail-fast) rather than silently retrying into the same broken state.
        let mut guard = slot.lock().expect("singleflight slot poisoned");

        if let Some(cached) = guard.as_ref() {
            return cached.clone();
        }

        // Step 3: first arriver — run the fetch, store the result, then clear inflight.
        let value = fetch();
        *guard = Some(value.clone());

        drop(guard);

        self.inflight.remove(key);

        value
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };
    use std::thread;

    use super::*;

    #[test]
    fn singleflight_deduplicates_concurrent_calls() {
        let sf: Arc<Singleflight<u32>> = Arc::new(Singleflight::new());
        let counter = Arc::new(AtomicUsize::new(0));

        let n = 16;
        let barrier = Arc::new(Barrier::new(n));

        let mut handles = Vec::new();

        for _ in 0..n {
            let sf = Arc::clone(&sf);
            let counter = Arc::clone(&counter);
            let barrier = Arc::clone(&barrier);

            handles.push(thread::spawn(move || {
                barrier.wait();

                sf.get_or_fetch("key1", || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    // Hold briefly so other threads arrive during the fetch.
                    thread::sleep(std::time::Duration::from_millis(30));
                    42u32
                })
            }));
        }

        for h in handles {
            assert_eq!(h.join().unwrap(), 42);
        }

        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "exactly one fetch should have run"
        );
    }

    #[test]
    fn singleflight_runs_new_fetch_after_completion() {
        let sf: Singleflight<u32> = Singleflight::new();
        let counter = AtomicUsize::new(0);

        let v1 = sf.get_or_fetch("key", || {
            counter.fetch_add(1, Ordering::SeqCst);
            7
        });

        let v2 = sf.get_or_fetch("key", || {
            counter.fetch_add(1, Ordering::SeqCst);
            9
        });

        assert_eq!(v1, 7);
        assert_eq!(v2, 9);
        assert_eq!(
            counter.load(Ordering::SeqCst),
            2,
            "fetch should run again after inflight entry was cleared"
        );
    }

    #[test]
    fn singleflight_different_keys_run_independently() {
        let sf: Arc<Singleflight<String>> = Arc::new(Singleflight::new());
        let counter = Arc::new(AtomicUsize::new(0));

        let n = 8;
        let barrier = Arc::new(Barrier::new(n));

        let mut handles = Vec::new();

        for i in 0..n {
            let sf = Arc::clone(&sf);
            let counter = Arc::clone(&counter);
            let barrier = Arc::clone(&barrier);

            let key = format!("k{}", i);

            handles.push(thread::spawn(move || {
                barrier.wait();

                sf.get_or_fetch(&key, || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    format!("v{}", i)
                })
            }));
        }

        for (i, h) in handles.into_iter().enumerate() {
            assert_eq!(h.join().unwrap(), format!("v{}", i));
        }

        assert_eq!(
            counter.load(Ordering::SeqCst),
            n,
            "each distinct key should trigger its own fetch"
        );
    }

    #[test]
    fn singleflight_returns_clone_of_value() {
        // Ensure cached value is cloned rather than moved, so concurrent
        // waiters can all receive it independently.
        let sf: Singleflight<Vec<u32>> = Singleflight::new();

        let v1 = sf.get_or_fetch("k", || vec![1, 2, 3]);
        // After completion inflight is cleared — this runs a new fetch.
        let v2 = sf.get_or_fetch("k", || vec![4, 5, 6]);

        assert_eq!(v1, vec![1, 2, 3]);
        assert_eq!(v2, vec![4, 5, 6]);
    }

    #[test]
    fn singleflight_option_clone_semantics() {
        // Regression-ish: Option<V: Clone> is the intended V for the populate
        // path. Confirm it round-trips values and None correctly.
        let sf: Singleflight<Option<String>> = Singleflight::new();

        let some = sf.get_or_fetch("a", || Some("hello".to_string()));
        let none = sf.get_or_fetch("b", || None);

        assert_eq!(some, Some("hello".to_string()));
        assert_eq!(none, None);
    }
}
