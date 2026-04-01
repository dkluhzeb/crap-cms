//! In-memory cache backend using DashMap.

use anyhow::Result;
use dashmap::DashMap;

use super::CacheBackend;

/// In-memory cache backed by a concurrent `DashMap`.
///
/// Enforces a soft cap on entries: once `max_entries` is reached, new
/// insertions are silently skipped until the cache is cleared.
pub struct MemoryCache {
    store: DashMap<String, Vec<u8>>,
    max_entries: usize,
}

impl MemoryCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            store: DashMap::new(),
            max_entries,
        }
    }
}

impl CacheBackend for MemoryCache {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.store.get(key).map(|v| v.value().clone()))
    }

    fn set(&self, key: &str, value: &[u8]) -> Result<()> {
        // Always allow overwrites of existing keys, even at capacity.
        // Only block genuinely new keys when at the soft cap.
        if self.store.len() < self.max_entries || self.store.contains_key(key) {
            self.store.insert(key.to_string(), value.to_vec());
        }

        Ok(())
    }

    fn delete(&self, key: &str) -> Result<()> {
        self.store.remove(key);
        Ok(())
    }

    fn clear(&self) -> Result<()> {
        self.store.clear();
        Ok(())
    }

    fn has(&self, key: &str) -> Result<bool> {
        Ok(self.store.contains_key(key))
    }

    fn kind(&self) -> &'static str {
        "memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_get_set() {
        let cache = MemoryCache::new(100);
        assert!(cache.get("k1").unwrap().is_none());

        cache.set("k1", b"hello").unwrap();
        assert_eq!(cache.get("k1").unwrap().unwrap(), b"hello");
    }

    #[test]
    fn delete_removes_key() {
        let cache = MemoryCache::new(100);
        cache.set("k1", b"v1").unwrap();
        cache.delete("k1").unwrap();
        assert!(cache.get("k1").unwrap().is_none());
    }

    #[test]
    fn delete_nonexistent_is_ok() {
        let cache = MemoryCache::new(100);
        assert!(cache.delete("nope").is_ok());
    }

    #[test]
    fn clear_removes_all() {
        let cache = MemoryCache::new(100);
        cache.set("k1", b"v1").unwrap();
        cache.set("k2", b"v2").unwrap();
        cache.clear().unwrap();
        assert!(cache.get("k1").unwrap().is_none());
        assert!(cache.get("k2").unwrap().is_none());
    }

    #[test]
    fn has_returns_correct_state() {
        let cache = MemoryCache::new(100);
        assert!(!cache.has("k1").unwrap());
        cache.set("k1", b"v1").unwrap();
        assert!(cache.has("k1").unwrap());
    }

    #[test]
    fn soft_cap_prevents_unbounded_growth() {
        let cache = MemoryCache::new(3);

        for i in 0..10 {
            cache.set(&format!("k{}", i), b"v").unwrap();
        }

        // Only first 3 should be stored
        assert!(cache.has("k0").unwrap());
        assert!(cache.has("k1").unwrap());
        assert!(cache.has("k2").unwrap());
        assert!(!cache.has("k3").unwrap());
    }

    #[test]
    fn overwrite_existing_key() {
        let cache = MemoryCache::new(100);
        cache.set("k1", b"old").unwrap();
        cache.set("k1", b"new").unwrap();
        assert_eq!(cache.get("k1").unwrap().unwrap(), b"new");
    }

    #[test]
    fn overwrite_existing_key_at_capacity() {
        let cache = MemoryCache::new(2);
        cache.set("k1", b"v1").unwrap();
        cache.set("k2", b"v2").unwrap();

        // At capacity — new keys should be rejected
        cache.set("k3", b"v3").unwrap();
        assert!(!cache.has("k3").unwrap());

        // But overwrites of existing keys must still work
        cache.set("k1", b"updated").unwrap();
        assert_eq!(cache.get("k1").unwrap().unwrap(), b"updated");
    }

    #[test]
    fn zero_max_entries_never_stores() {
        let cache = MemoryCache::new(0);
        cache.set("k1", b"v1").unwrap();
        assert!(!cache.has("k1").unwrap());
        assert!(cache.get("k1").unwrap().is_none());
    }

    #[test]
    fn kind_returns_memory() {
        let cache = MemoryCache::new(100);
        assert_eq!(cache.kind(), "memory");
    }
}
