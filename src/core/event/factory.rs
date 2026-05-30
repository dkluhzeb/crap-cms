//! Factory functions that build [`SharedEventTransport`] and
//! [`SharedInvalidationTransport`] from config.

use std::sync::Arc;

use anyhow::Result;
#[cfg(not(feature = "redis"))]
use anyhow::bail;
use tracing::info;

use crate::config::{LiveConfig, LiveTransport};
use crate::core::event::{
    InProcessEventBus, InProcessInvalidationBus, SharedEventTransport, SharedInvalidationTransport,
};

/// Build the event transport appropriate for `live.transport` + the shared
/// `redis_url` from the cache config (when `transport = "redis"`).
///
/// Returns `None` when live updates are disabled (`live.enabled = false`).
///
/// # Errors
///
/// Returns an error if the transport name is unknown, or the Redis transport
/// fails to initialize.
pub fn create_event_transport(
    live: &LiveConfig,
    redis_url: &str,
) -> Result<Option<SharedEventTransport>> {
    if !live.enabled {
        info!("Live event streaming disabled");
        return Ok(None);
    }

    match live.transport {
        LiveTransport::Memory => {
            info!(
                "Using in-process event transport (capacity: {})",
                live.channel_capacity
            );

            Ok(Some(Arc::new(InProcessEventBus::new(
                live.channel_capacity,
            ))))
        }
        #[cfg(feature = "redis")]
        LiveTransport::Redis => {
            info!(url = %redis_url, "Using Redis event transport");

            Ok(Some(Arc::new(
                super::redis_transport::RedisEventTransport::new(redis_url)?,
            )))
        }
        #[cfg(not(feature = "redis"))]
        LiveTransport::Redis => {
            let _ = redis_url;
            bail!(
                "Redis event transport requires the `redis` feature. \
                 Rebuild with `--features redis`, or set `[live] transport = \"memory\"`."
            );
        }
    }
}

/// Build the invalidation transport using the same transport selection as
/// [`create_event_transport`]. Operators don't configure the two separately.
///
/// # Errors
///
/// Returns an error if the transport name is unknown, or the Redis transport
/// fails to initialize.
pub fn create_invalidation_transport(
    live: &LiveConfig,
    redis_url: &str,
) -> Result<SharedInvalidationTransport> {
    match live.transport {
        LiveTransport::Memory => Ok(Arc::new(InProcessInvalidationBus::new())),
        #[cfg(feature = "redis")]
        LiveTransport::Redis => Ok(Arc::new(
            super::redis_transport::RedisInvalidationTransport::new(redis_url)?,
        )),
        #[cfg(not(feature = "redis"))]
        LiveTransport::Redis => {
            let _ = redis_url;
            bail!(
                "Redis invalidation transport requires the `redis` feature. \
                 Rebuild with `--features redis`, or set `[live] transport = \"memory\"`."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_transport_is_default() {
        let cfg = LiveConfig::default();
        let transport = create_event_transport(&cfg, "redis://127.0.0.1:6379")
            .unwrap()
            .expect("enabled");
        assert_eq!(transport.kind(), "in_process");
    }

    #[test]
    fn disabled_live_returns_none() {
        let cfg = LiveConfig {
            enabled: false,
            ..LiveConfig::default()
        };
        let transport = create_event_transport(&cfg, "").unwrap();
        assert!(transport.is_none());
    }

    #[test]
    fn invalidation_transport_memory_default() {
        let cfg = LiveConfig::default();
        let t = create_invalidation_transport(&cfg, "").unwrap();
        assert_eq!(t.kind(), "in_process");
    }

    #[cfg(not(feature = "redis"))]
    #[test]
    fn redis_without_feature_errors() {
        let cfg = LiveConfig {
            transport: LiveTransport::Redis,
            ..LiveConfig::default()
        };
        let Err(err) = create_event_transport(&cfg, "") else {
            panic!("expected error when redis feature is disabled");
        };
        assert!(
            err.to_string().contains("redis` feature"),
            "unexpected error: {err}"
        );
    }

    #[cfg(not(feature = "redis"))]
    #[test]
    fn redis_invalidation_without_feature_errors() {
        let cfg = LiveConfig {
            transport: LiveTransport::Redis,
            ..LiveConfig::default()
        };
        let Err(err) = create_invalidation_transport(&cfg, "") else {
            panic!("expected error when redis feature is disabled");
        };
        assert!(err.to_string().contains("redis` feature"));
    }
}
