//! Rate limit backend factory.

use std::sync::Arc;

use anyhow::{Result, bail};
use tracing::info;

use super::{MemoryRateLimitBackend, NoneRateLimitBackend, SharedRateLimitBackend};

/// Inputs for [`create_rate_limit_backend`]. Grouping the parameters
/// keeps the redis-only fields out of the call signature when the
/// `redis` feature is off — public struct fields don't trigger
/// unused-warnings the way unused fn parameters do.
pub struct RateLimitFactoryConfig<'a> {
    pub backend: &'a str,
    pub redis_url: &'a str,
    pub prefix: &'a str,
}

/// Create the appropriate rate limit backend from config.
pub fn create_rate_limit_backend(
    cfg: &RateLimitFactoryConfig<'_>,
) -> Result<SharedRateLimitBackend> {
    match cfg.backend {
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
            info!(
                url = %cfg.redis_url,
                prefix = %cfg.prefix,
                "Using Redis rate limit backend"
            );
            Ok(Arc::new(super::redis::RedisRateLimitBackend::new(
                cfg.redis_url,
                cfg.prefix,
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

    fn cfg(backend: &str) -> RateLimitFactoryConfig<'_> {
        RateLimitFactoryConfig {
            backend,
            redis_url: "",
            prefix: "",
        }
    }

    #[test]
    fn create_memory_backend() {
        let backend = create_rate_limit_backend(&cfg("memory")).unwrap();
        assert_eq!(backend.kind(), "memory");
    }

    #[test]
    fn create_none_backend() {
        let backend = create_rate_limit_backend(&cfg("none")).unwrap();
        assert_eq!(backend.kind(), "none");
    }

    #[test]
    fn create_unknown_backend_errors() {
        let result = create_rate_limit_backend(&cfg("memcached"));
        assert!(result.is_err());
    }
}
