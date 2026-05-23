//! Background job scheduler configuration.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::config::parsing::{serde_duration, serde_duration_option};

/// Default concurrency cap applied to the `images` queue when the
/// operator doesn't set `[jobs.queues.images]` explicitly. Image
/// encoding (AVIF / WebP) is CPU-bound — each encoder pins one core
/// for several seconds, so leaving it uncapped lets a busy upload
/// burst saturate every core on the host. `2` is conservative enough
/// to stay responsive on small servers while still making forward
/// progress.
const DEFAULT_IMAGES_QUEUE_CONCURRENCY: u32 = 2;

/// Per-queue scheduling knobs. Keyed by queue name in
/// [`JobsConfig::queues`].
///
/// Currently just `concurrency` — the max number of jobs (across all
/// slugs) that can run concurrently in this queue. Reserved field for
/// future extensions (`paused`, `rate_limit`, …) — the inline TOML
/// form `default = { concurrency = 10 }` stays forward-compatible.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueueConfig {
    /// Max concurrent runs across all slugs in this queue. `0` =
    /// unlimited (only the global `max_concurrent` and per-slug
    /// `JobDefinition::concurrency` apply). Default: `0`.
    pub concurrency: u32,
}

/// Background job scheduler configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct JobsConfig {
    /// Maximum concurrent jobs in flight across the whole cluster.
    /// Enforced per poll tick by querying
    /// `SELECT COUNT(*) FROM _crap_jobs WHERE status = 'running'`
    /// from the shared DB, so all servers observe the same total.
    /// With identical configs across servers, behaves as a single
    /// cluster cap (not multiplied by server count). Default: 10.
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
    /// Priority decay in seconds: the wait time required for a job's
    /// effective scheduling priority to bump by `+1`. `0` (default)
    /// disables decay — jobs are ordered purely by static
    /// `priority DESC, created_at ASC` (index-friendly). Set to a
    /// positive duration (`"1m"`, `"30s"`, `"1h"`) to enable
    /// aging-based promotion so older lower-priority jobs eventually
    /// get claimed instead of starving forever behind a backlog of
    /// higher-priority work.
    #[serde(with = "serde_duration")]
    pub priority_decay: u64,
    /// Per-queue scheduling overrides, keyed by queue name. Operators
    /// declare aggregate concurrency caps for resource-shared work
    /// (e.g., a shared SMTP pool's `emails` queue). All caps are
    /// cluster-wide; composition is strictest-wins across
    /// `max_concurrent` (global), `queues[name].concurrency`
    /// (per-queue), and `JobDefinition::concurrency` (per-slug).
    /// Queue names without an entry are unconstrained beyond the
    /// global cap.
    ///
    /// Framework-supplied defaults are applied at config load by
    /// [`Self::apply_queue_defaults`] — currently just `images = {
    /// concurrency = 2 }` so AVIF / WebP encoders don't pin every
    /// core during an upload burst. Operator overrides win.
    pub queues: HashMap<String, QueueConfig>,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 10,
            poll_interval: 1,
            cron_interval: 60,
            heartbeat_interval: 10,
            auto_purge: Some(7 * 86400), // 7 days
            priority_decay: 0,
            queues: HashMap::new(),
        }
    }
}

impl JobsConfig {
    /// Apply framework defaults for queues the operator didn't set.
    ///
    /// Currently seeds only the `images` queue with a conservative
    /// concurrency cap (see [`DEFAULT_IMAGES_QUEUE_CONCURRENCY`]).
    /// Explicit operator overrides win — `or_insert` is a no-op when
    /// the key exists.
    ///
    /// Called once from `CrapConfig::load` after deserialization so
    /// downstream callers see a fully populated `queues` map.
    pub fn apply_queue_defaults(&mut self) {
        self.queues
            .entry("images".to_string())
            .or_insert(QueueConfig {
                concurrency: DEFAULT_IMAGES_QUEUE_CONCURRENCY,
            });
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
        // `queues` is empty in pure Default; `apply_queue_defaults` is
        // what seeds the framework defaults at load time.
        assert!(cfg.queues.is_empty());
    }

    #[test]
    fn apply_queue_defaults_seeds_images() {
        let mut cfg = JobsConfig::default();
        cfg.apply_queue_defaults();
        assert_eq!(
            cfg.queues.get("images").map(|q| q.concurrency),
            Some(DEFAULT_IMAGES_QUEUE_CONCURRENCY)
        );
    }

    #[test]
    fn apply_queue_defaults_preserves_operator_override() {
        let mut cfg = JobsConfig::default();
        cfg.queues
            .insert("images".to_string(), QueueConfig { concurrency: 8 });
        cfg.apply_queue_defaults();
        // Explicit operator value wins.
        assert_eq!(cfg.queues["images"].concurrency, 8);
    }

    #[test]
    fn apply_queue_defaults_preserves_other_queues() {
        let mut cfg = JobsConfig::default();
        cfg.queues
            .insert("emails".to_string(), QueueConfig { concurrency: 4 });
        cfg.apply_queue_defaults();
        assert_eq!(cfg.queues["emails"].concurrency, 4);
        // `images` default still applied.
        assert_eq!(
            cfg.queues["images"].concurrency,
            DEFAULT_IMAGES_QUEUE_CONCURRENCY
        );
    }

    #[test]
    fn loaded_config_has_images_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Empty TOML — operator set nothing.
        std::fs::write(tmp.path().join("crap.toml"), "").unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(
            config.jobs.queues["images"].concurrency,
            DEFAULT_IMAGES_QUEUE_CONCURRENCY
        );
    }

    #[test]
    fn loaded_config_operator_overrides_images() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs.queues.images]\nconcurrency = 8\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.queues["images"].concurrency, 8);
    }

    #[test]
    fn priority_decay_disabled_by_default() {
        let cfg = JobsConfig::default();
        assert_eq!(cfg.priority_decay, 0);
    }

    #[test]
    fn priority_decay_parses_minute_string() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs]\npriority_decay = \"1m\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.priority_decay, 60);
    }

    #[test]
    fn priority_decay_parses_seconds_integer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs]\npriority_decay = 30\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.priority_decay, 30);
    }

    #[test]
    fn priority_decay_parses_hour_string() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs]\npriority_decay = \"1h\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.priority_decay, 3600);
    }

    // ── queues ──────────────────────────────────────────────────────

    #[test]
    fn queues_default_empty() {
        let cfg = JobsConfig::default();
        assert!(cfg.queues.is_empty());
    }

    #[test]
    fn queues_parses_inline_form() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs.queues]\n\
             default = { concurrency = 10 }\n\
             emails = { concurrency = 4 }\n\
             images = { concurrency = 2 }\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.queues.len(), 3);
        assert_eq!(config.jobs.queues["default"].concurrency, 10);
        assert_eq!(config.jobs.queues["emails"].concurrency, 4);
        assert_eq!(config.jobs.queues["images"].concurrency, 2);
    }

    #[test]
    fn queues_parses_block_form() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs.queues.emails]\nconcurrency = 4\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.queues["emails"].concurrency, 4);
    }

    /// Regression: the removed `[jobs] image_concurrency` field
    /// surfaces a clear "unknown field" error for operators who carry
    /// it forward from earlier alpha builds — they should land on the
    /// CHANGELOG entry / docs that explain the rename to
    /// `[jobs.queues.images] concurrency = N`.
    #[test]
    fn removed_image_concurrency_field_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs]\nimage_concurrency = 4\n",
        )
        .unwrap();
        let err = crate::config::CrapConfig::load(tmp.path()).unwrap_err();
        let chain = format!("{err:#}");
        assert!(
            chain.contains("image_concurrency"),
            "expected error to mention the removed `image_concurrency` field; got: {chain}"
        );
    }

    #[test]
    fn queue_unknown_field_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs.queues.emails]\nconcurrency = 4\nbogus_field = true\n",
        )
        .unwrap();
        let err = crate::config::CrapConfig::load(tmp.path()).unwrap_err();
        // Walk the anyhow chain; the bogus-field message is on the
        // serde-level inner error.
        let chain = format!("{err:#}");
        assert!(
            chain.contains("bogus_field"),
            "expected unknown-field error mentioning 'bogus_field'; got chain: {chain}"
        );
    }
}
