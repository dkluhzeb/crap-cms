//! Background job scheduler configuration.

use serde::{Deserialize, Serialize};

use crate::config::parsing::{serde_duration, serde_duration_option};

/// Background job scheduler configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct JobsConfig {
    /// Max concurrent job executions across all queues. Default: 10.
    pub max_concurrent: usize,
    /// How often to poll for pending jobs, in seconds. Default: 1s.
    /// Accepts integer seconds or human-readable string ("1s", "5s").
    #[serde(with = "serde_duration")]
    pub poll_interval: u64,
    /// How often to check cron schedules, in seconds. Default: 60s.
    #[serde(with = "serde_duration")]
    pub cron_interval: u64,
    /// How often to update heartbeat for running jobs, in seconds. Default: 10s.
    #[serde(with = "serde_duration")]
    pub heartbeat_interval: u64,
    /// Auto-purge completed/failed jobs older than this duration (in seconds).
    /// Accepts integer seconds or human-readable string ("7d", "24h").
    /// None disables auto-purge.
    #[serde(with = "serde_duration_option")]
    pub auto_purge: Option<u64>,
    /// Number of pending image conversions to process per scheduler poll. Default: 10.
    pub image_queue_batch_size: usize,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 10,
            poll_interval: 1,
            cron_interval: 60,
            heartbeat_interval: 10,
            auto_purge: Some(7 * 86400), // 7 days
            image_queue_batch_size: 10,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_purge_default_config() {
        let cfg = JobsConfig::default();
        assert_eq!(cfg.auto_purge, Some(7 * 86400));
    }

    #[test]
    fn jobs_config_defaults() {
        let cfg = JobsConfig::default();
        assert_eq!(cfg.max_concurrent, 10);
        assert_eq!(cfg.poll_interval, 1);
        assert_eq!(cfg.cron_interval, 60);
        assert_eq!(cfg.heartbeat_interval, 10);
        assert_eq!(cfg.auto_purge, Some(7 * 86400));
        assert_eq!(cfg.image_queue_batch_size, 10);
    }

    #[test]
    fn jobs_image_queue_batch_size_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs]\nimage_queue_batch_size = 50\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.image_queue_batch_size, 50);
    }
}
