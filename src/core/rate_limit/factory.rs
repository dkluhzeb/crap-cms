//! Rate limit backend factory.

use std::sync::Arc;

use anyhow::{Result, bail};
use tracing::info;

use super::{MemoryRateLimitBackend, NoneRateLimitBackend, SharedRateLimitBackend};

/// Create the appropriate rate limit backend from config.
pub fn create_rate_limit_backend(
    backend: &str,
    #[allow(unused_variables)] redis_url: &str,
    #[allow(unused_variables)] prefix: &str,
) -> Result<SharedRateLimitBackend> {
    match backend {
        "memory" | "" => {
            info!("Using memory rate limit backend");
            Ok(Arc::new(MemoryRateLimitBackend::new()))
        }
        "none" => {
            info!("Rate limiting disabled (none backend)");
            Ok(Arc::new(NoneRateLimitBackend))
        }
        #[cfg(feature = "redis")]
        "redis" => {
            info!(url = %redis_url, prefix = %prefix, "Using Redis rate limit backend");
            Ok(Arc::new(super::redis::RedisRateLimitBackend::new(
                redis_url, prefix,
            )?))
        }
        #[cfg(not(feature = "redis"))]
        "redis" => {
            bail!(
                "Redis rate limit backend requires the `redis` feature. \
                 Rebuild with `--features redis`."
            );
        }
        other => bail!("Unknown rate limit backend: '{}'", other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_memory_backend() {
        let backend = create_rate_limit_backend("memory", "", "").unwrap();
        assert_eq!(backend.kind(), "memory");
    }

    #[test]
    fn create_none_backend() {
        let backend = create_rate_limit_backend("none", "", "").unwrap();
        assert_eq!(backend.kind(), "none");
    }

    #[test]
    fn create_unknown_backend_errors() {
        let result = create_rate_limit_backend("memcached", "", "");
        assert!(result.is_err());
    }
}
