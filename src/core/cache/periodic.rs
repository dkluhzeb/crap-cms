//! Periodic full cache clear.

use std::time::Duration;

use tokio::{select, spawn, time::interval};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::core::cache::SharedCache;

/// Cadence of the periodic full clear, or `None` when it is disabled
/// (`max_age_secs = 0`), has nothing to clear (the `none` backend), or is
/// already covered: `redis` entries expire after `max_age_secs` on their own,
/// and wiping a store every node shares once per node would cut entry lifetime
/// by the node count.
#[must_use]
pub fn periodic_clear_interval(cache: &SharedCache, max_age_secs: u64) -> Option<Duration> {
    let needs_clear = !matches!(cache.kind(), "none" | "redis");

    (max_age_secs > 0 && needs_clear).then(|| Duration::from_secs(max_age_secs))
}

/// Spawn a task that clears `cache` every `every` until `shutdown` fires.
///
/// Bounds the staleness left by database mutations that bypass the service
/// layer's invalidation. Every process that holds a cache must run one —
/// admin-only nodes and workers included — because a memory cache lives in
/// its own process: a clear on one node does nothing for another's.
pub fn spawn_periodic_clear(cache: SharedCache, every: Duration, shutdown: CancellationToken) {
    spawn(async move {
        let mut tick = interval(every);

        tick.tick().await; // skip first immediate tick

        loop {
            select! {
                _ = tick.tick() => {
                    if let Err(e) = cache.clear() {
                        warn!("Periodic cache clear failed: {:#}", e);
                    }
                },
                () = shutdown.cancelled() => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::time::sleep;

    use anyhow::Result;

    use super::*;
    use crate::core::cache::{CacheBackend, MemoryCache, NoneCache};

    /// A backend that only reports a kind, for the interval decision.
    struct KindOnly(&'static str);

    impl CacheBackend for KindOnly {
        fn get(&self, _key: &str) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn set(&self, _key: &str, _value: &[u8]) -> Result<()> {
            Ok(())
        }

        fn delete(&self, _key: &str) -> Result<()> {
            Ok(())
        }

        fn clear(&self) -> Result<()> {
            Ok(())
        }

        fn has(&self, _key: &str) -> Result<bool> {
            Ok(false)
        }

        fn kind(&self) -> &'static str {
            self.0
        }
    }

    #[test]
    fn interval_is_off_for_zero_max_age_and_self_expiring_backends() {
        let memory: SharedCache = Arc::new(MemoryCache::new(10));
        let none: SharedCache = Arc::new(NoneCache);
        let redis: SharedCache = Arc::new(KindOnly("redis"));

        assert_eq!(periodic_clear_interval(&memory, 0), None);
        assert_eq!(periodic_clear_interval(&none, 60), None);
        assert_eq!(
            periodic_clear_interval(&redis, 60),
            None,
            "redis entries already expire after max_age_secs"
        );
        assert_eq!(
            periodic_clear_interval(&memory, 90),
            Some(Duration::from_secs(90))
        );
    }

    #[tokio::test]
    async fn the_task_clears_the_cache_until_shutdown() {
        let cache: SharedCache = Arc::new(MemoryCache::new(10));
        let shutdown = CancellationToken::new();

        cache.set("k", b"v").unwrap();
        spawn_periodic_clear(
            Arc::clone(&cache),
            Duration::from_millis(20),
            shutdown.clone(),
        );
        sleep(Duration::from_millis(100)).await;

        assert!(
            !cache.has("k").unwrap(),
            "a tick must have cleared the entry"
        );

        shutdown.cancel();
        sleep(Duration::from_millis(40)).await;
        cache.set("k", b"v").unwrap();
        sleep(Duration::from_millis(60)).await;

        assert!(cache.has("k").unwrap(), "no clear may run after shutdown");
    }
}
