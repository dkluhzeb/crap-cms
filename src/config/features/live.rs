//! Live event streaming configuration (SSE + gRPC Subscribe).

use serde::{Deserialize, Serialize};

/// Live event streaming configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LiveConfig {
    /// Enable live event streaming (SSE + gRPC Subscribe). Default: true.
    pub enabled: bool,
    /// Default event delivery mode for collections/globals that don't specify one.
    /// `"metadata"` (default) = id/operation only, `"full"` = `after_read` hooks + data.
    pub default_mode: String,
    /// Event transport. `"memory"` (default) -- in-process broadcast, events do
    /// not cross server nodes. `"redis"` -- Redis pub/sub (requires the `redis`
    /// feature); events fan out to all server nodes subscribed to the same
    /// Redis instance. The Redis URL is reused from `[cache] redis_url`.
    #[serde(default = "default_live_transport")]
    pub transport: String,
    /// Broadcast channel capacity. Default: 1024.
    pub channel_capacity: usize,
    /// Maximum concurrent SSE connections (admin UI). 0 = unlimited. Default: 1000.
    pub max_sse_connections: usize,
    /// Maximum concurrent gRPC Subscribe streams. 0 = unlimited. Default: 1000.
    pub max_subscribe_connections: usize,
    /// Per-subscriber outbound send timeout (milliseconds). If forwarding an
    /// event to a specific live-update client (gRPC Subscribe or admin SSE)
    /// takes longer than this, that subscriber is dropped. Guards against a
    /// slow client holding broadcast capacity and starving other subscribers.
    /// Default: 1000 (1 second).
    pub subscriber_send_timeout_ms: u64,
}

fn default_live_transport() -> String {
    "memory".to_string()
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_mode: "metadata".to_string(),
            transport: default_live_transport(),
            channel_capacity: 1024,
            max_sse_connections: 1000,
            max_subscribe_connections: 1000,
            subscriber_send_timeout_ms: 1000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_config_defaults() {
        let live = LiveConfig::default();
        assert!(live.enabled);
        assert_eq!(live.transport, "memory");
        assert_eq!(live.channel_capacity, 1024);
        assert_eq!(live.max_sse_connections, 1000);
        assert_eq!(live.max_subscribe_connections, 1000);
        assert_eq!(live.subscriber_send_timeout_ms, 1000);
    }

    #[test]
    fn live_config_transport_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[live]\ntransport = \"redis\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.live.transport, "redis");
    }
}
