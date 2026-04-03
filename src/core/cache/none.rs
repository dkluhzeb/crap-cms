//! No-op cache backend — all operations are silent no-ops.

use anyhow::Result;

use crate::core::cache::CacheBackend;

/// No-op cache that never stores or returns data.
///
/// Used when caching is disabled (`cache.backend = "none"`).
pub struct NoneCache;

impl CacheBackend for NoneCache {
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
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_always_none() {
        let cache = NoneCache;
        assert!(cache.get("any-key").unwrap().is_none());
    }

    #[test]
    fn set_and_get_returns_none() {
        let cache = NoneCache;
        cache.set("k1", b"v1").unwrap();
        assert!(cache.get("k1").unwrap().is_none());
    }

    #[test]
    fn operations_dont_error() {
        let cache = NoneCache;
        assert!(cache.set("k", b"v").is_ok());
        assert!(cache.delete("k").is_ok());
        assert!(cache.clear().is_ok());
        assert!(cache.has("k").is_ok());
    }

    #[test]
    fn has_always_false() {
        let cache = NoneCache;
        assert!(!cache.has("k").unwrap());
    }

    #[test]
    fn kind_returns_none() {
        let cache = NoneCache;
        assert_eq!(cache.kind(), "none");
    }
}
