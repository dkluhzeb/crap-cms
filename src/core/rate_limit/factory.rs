//! Rate limit backend factory.

use std::sync::Arc;

use anyhow::Result;
#[cfg(not(feature = "redis"))]
use anyhow::bail;
use tracing::info;

use crate::config::RateLimitBackend;

use super::{MemoryRateLimitBackend, NoneRateLimitBackend, SharedRateLimitBackend};

/// Inputs for [`create_rate_limit_backend`]. Grouping the parameters
/// keeps the redis-only fields out of the call signature when the
/// `redis` feature is off — public struct fields don't trigger
/// unused-warnings the way unused fn parameters do.
pub struct RateLimitFactoryConfig<'a> {
    pub backend: RateLimitBackend,
    pub redis_url: &'a str,
    pub prefix: &'a str,
}

/// Create the appropriate rate limit backend from config.
///
/// # Errors
///
/// Returns an error if the Redis client fails to initialize (or the Redis
/// backend is selected without the `redis` feature).
pub fn create_rate_limit_backend(
    cfg: &RateLimitFactoryConfig<'_>,
) -> Result<SharedRateLimitBackend> {
    match cfg.backend {
        RateLimitBackend::Memory => {
            info!("Using memory rate limit backend");
            Ok(Arc::new(MemoryRateLimitBackend::new()))
        }
        RateLimitBackend::None => {
            info!("Rate limiting disabled (none backend)");
            Ok(Arc::new(NoneRateLimitBackend))
        }
        #[cfg(feature = "redis")]
        RateLimitBackend::Redis => {
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
        RateLimitBackend::Redis => {
            bail!(
                "Redis rate limit backend requires the `redis` feature. \
                 Rebuild with `--features redis`."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(backend: RateLimitBackend) -> RateLimitFactoryConfig<'static> {
        RateLimitFactoryConfig {
            backend,
            redis_url: "",
            prefix: "",
        }
    }

    #[test]
    fn create_memory_backend() {
        let backend = create_rate_limit_backend(&cfg(RateLimitBackend::Memory)).unwrap();
        assert_eq!(backend.kind(), "memory");
    }

    #[test]
    fn create_none_backend() {
        let backend = create_rate_limit_backend(&cfg(RateLimitBackend::None)).unwrap();
        assert_eq!(backend.kind(), "none");
    }
}
