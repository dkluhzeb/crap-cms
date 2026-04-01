//! No-op rate limit backend — all operations are silent no-ops.

use anyhow::Result;

use super::RateLimitBackend;

/// No-op rate limit backend. Always returns count=0.
/// Used when rate limiting is disabled.
pub struct NoneRateLimitBackend;

impl RateLimitBackend for NoneRateLimitBackend {
    fn count(&self, _key: &str, _window_secs: u64) -> Result<u32> {
        Ok(0)
    }

    fn record(&self, _key: &str, _window_secs: u64) -> Result<()> {
        Ok(())
    }

    fn check_and_record(&self, _key: &str, _max_count: u32, _window_secs: u64) -> Result<bool> {
        Ok(true)
    }

    fn clear(&self, _key: &str) -> Result<()> {
        Ok(())
    }

    fn kind(&self) -> &'static str {
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_always_zero() {
        let backend = NoneRateLimitBackend;
        backend.record("k1", 60).unwrap();
        backend.record("k1", 60).unwrap();
        assert_eq!(backend.count("k1", 60).unwrap(), 0);
    }

    #[test]
    fn operations_dont_error() {
        let backend = NoneRateLimitBackend;
        assert!(backend.record("k", 60).is_ok());
        assert!(backend.clear("k").is_ok());
    }
}
