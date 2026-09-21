//! Background job scheduler configuration.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    config::parsing::{serde_duration, serde_duration_option},
    core::{email::SYSTEM_EMAIL_QUEUE, job::SYSTEM_BULK_QUEUE, upload::IMAGE_CONVERT_QUEUE},
};

/// Default concurrency cap applied to the `images` queue when the
/// operator doesn't set `[jobs.queues.images]` explicitly. Image
/// encoding (AVIF / WebP) is CPU-bound — each encoder pins one core
/// for several seconds, so leaving it uncapped lets a busy upload
/// burst saturate every core on the host. `2` is conservative enough
/// to stay responsive on small servers while still making forward
/// progress.
const DEFAULT_IMAGES_QUEUE_CONCURRENCY: u32 = 2;

/// Default timeout (seconds) applied to the `images` queue when the
/// operator doesn't set `[jobs.queues.images]` explicitly. Used for
/// `_system_image_convert` jobs — large AVIF encodes can take 30-60s
/// on commodity hardware; the default gives generous headroom for
/// big originals on slow disks. Override via
/// `[jobs.queues.images] timeout = "..."`.
pub(crate) const DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS: u64 = 300;

/// Default retry budget applied to the `images` queue when the
/// operator doesn't set `[jobs.queues.images]` explicitly. `2`
/// retries = 3 total attempts, matching the historical hardcoded
/// `DEFAULT_MAX_ATTEMPTS = 3`. Transient encoder failures
/// (memory pressure, disk contention) often succeed on retry;
/// persistent failures (corrupt source, unsupported pixel format)
/// fail through after the budget is exhausted.
const DEFAULT_IMAGES_QUEUE_RETRIES: u32 = 2;

/// Default concurrency cap applied to the `email` queue when the
/// operator doesn't set `[jobs.queues.email]` explicitly. SMTP
/// providers throttle on burst; `5` is the historical default from
/// the now-removed `[email] queue_concurrency` field.
const DEFAULT_EMAIL_QUEUE_CONCURRENCY: u32 = 5;

/// `bulk` queue: one at a time — each run holds a write transaction for its
/// whole batch.
const DEFAULT_BULK_QUEUE_CONCURRENCY: u32 = 1;
/// `bulk` queue: generous — large batches are the reason to queue at all.
pub(crate) const DEFAULT_BULK_QUEUE_TIMEOUT_SECS: u64 = 3600;
/// `bulk` queue: no automatic retries (see the seeding comment).
const DEFAULT_BULK_QUEUE_RETRIES: u32 = 0;

/// Default timeout (seconds) applied to the `email` queue when the
/// operator doesn't set `[jobs.queues.email]` explicitly. SMTP
/// handshake + delivery within `30s` is the historical default from
/// the now-removed `[email] queue_timeout` field.
pub(crate) const DEFAULT_EMAIL_QUEUE_TIMEOUT_SECS: u64 = 30;

/// Extra wall-clock the scheduler's outer timer allows a job that enforces its
/// own cooperative deadline (`_system_bulk` aborts and rolls back at its
/// `timeout`), covering the post-commit work — event publishing, upload-file
/// deletion — that happens after the last in-batch deadline check.
///
/// Lives here rather than next to the scheduler because the drain deadline is
/// derived from it and config must not depend on the scheduler.
pub(crate) const SELF_LIMITING_JOB_GRACE_SECS: u64 = 300;

/// Extra wall-clock a graceful stop allows on top of the longest a single
/// job run can legitimately take. Covers the terminal status write (and, for
/// a self-limiting run, the rollback) that still has to happen after a run
/// hits its own deadline. One number for `serve`, the standalone worker, and
/// `work --stop`, so a stop can neither outrun nor undercut a running job.
///
/// It is the self-limiting grace itself, not an independent number: a
/// self-limiting run's outer timer already runs to `timeout +
/// SELF_LIMITING_JOB_GRACE_SECS`, so anything shorter would abandon a run that
/// hit its cooperative deadline and is still winding down — the two numbers
/// drifting apart is exactly the bug this shares one constant to prevent.
pub(crate) const JOB_DRAIN_GRACE_SECS: u64 = SELF_LIMITING_JOB_GRACE_SECS;

/// Default retry budget applied to the `email` queue when the
/// operator doesn't set `[jobs.queues.email]` explicitly. `3` retries
/// = 4 total attempts, matching the historical default from the
/// now-removed `[email] queue_retries` field. Transient SMTP
/// failures (greylisting, brief network blips) typically clear within
/// the retry budget.
const DEFAULT_EMAIL_QUEUE_RETRIES: u32 = 3;

/// Per-queue scheduling knobs. Keyed by queue name in
/// [`JobsConfig::queues`].
///
/// Forward-compatible inline TOML form: `default = { concurrency =
/// 10, timeout = "5m" }`. Reserved for future extensions
/// (`paused`, `rate_limit`, …).
///
/// **Fields are `Option<T>` so partial operator overrides don't drop
/// framework defaults.** `None` means "field not specified by the
/// operator — inherit the framework default if there is one."
/// `Some(0)` means "operator explicitly chose unlimited / no
/// timeout." This distinction matters because
/// [`JobsConfig::apply_queue_defaults`] seeds well-known queues
/// (`images`) with safe defaults; without `Option`, an operator
/// writing only `concurrency = 4` would silently lose the framework
/// timeout default.
#[derive(Debug, Clone, Default, Deserialize, Serialize, crap_cms_macros::ConfigKeys)]
#[serde(default, deny_unknown_fields)]
pub struct QueueConfig {
    /// Max concurrent runs across all slugs in this queue.
    /// `None` = inherit framework default if any. `Some(0)` =
    /// operator-chosen unlimited (no per-queue cap; only global
    /// `max_concurrent` and per-slug caps apply). `Some(N)` = cap N.
    pub concurrency: Option<u32>,
    /// Per-queue timeout in seconds. Used for jobs whose
    /// `JobDefinition::timeout` isn't set explicitly — primarily
    /// **system jobs** (`_system_image_convert`, `_system_email`)
    /// which have no Lua declaration. User jobs with their own
    /// `crap.jobs.define({ timeout = N })` keep their declared value.
    ///
    /// `None` = inherit framework default if any. `Some(0)` =
    /// operator-chosen "no per-queue timeout" (system jobs in this
    /// queue fall back to a hard-coded scheduler default).
    ///
    /// Accepts integer seconds or human-readable string (`"30s"`,
    /// `"2m"`, `"1h"`).
    #[serde(with = "serde_duration_option")]
    pub timeout: Option<u64>,
    /// Per-queue retry budget — number of retries after the initial
    /// attempt (total attempts = `retries + 1`).
    ///
    /// **Reach (alpha.9):** consumed by every system job —
    /// `_system_image_convert` via
    /// [`JobsConfig::system_image_max_attempts`] and `_system_email`
    /// via [`JobsConfig::system_email_max_attempts`]. User Lua jobs
    /// always use their `JobDefinition.retries` (set at
    /// `crap.jobs.define` time); per-call overrides
    /// (`crap.email.queue{ retries = N }`) still win.
    ///
    /// `None` = inherit framework default if any. `Some(0)` = one
    /// attempt, no retries. `Some(N)` = `N` retries.
    pub retries: Option<u32>,
}

impl QueueConfig {
    /// Effective concurrency cap with `0` = unlimited.
    #[must_use]
    pub fn effective_concurrency(&self) -> u32 {
        self.concurrency.unwrap_or(0)
    }

    /// Effective timeout in seconds with `0` = no per-queue timeout
    /// (callers should fall back to per-job or hard-coded defaults).
    #[must_use]
    pub fn effective_timeout(&self) -> u64 {
        self.timeout.unwrap_or(0)
    }

    /// Effective `max_attempts` for system jobs in this queue
    /// (retries + 1). Returns `None` when neither operator config
    /// nor framework default applies — caller falls back to a
    /// hardcoded constant.
    #[must_use]
    pub fn effective_max_attempts(&self) -> Option<u32> {
        self.retries.map(|r| r.saturating_add(1))
    }
}

/// Background job scheduler configuration.
#[derive(Debug, Clone, Deserialize, Serialize, crap_cms_macros::ConfigKeys)]
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
    /// [`Self::apply_queue_defaults`] — seeds the `images` and `email`
    /// queues (the two framework-owned queues that host system jobs
    /// without a `JobDefinition` to carry per-job defaults). User
    /// queues are NOT seeded: jobs defined via `crap.jobs.define{
    /// queue = "...", retries = …, timeout = … }` carry their defaults
    /// on the `JobDefinition`, so an unconfigured user queue stays
    /// unconstrained beyond the global `max_concurrent`. Each field
    /// is merged independently — partial operator overrides keep the
    /// framework defaults for unspecified fields.
    pub queues: HashMap<String, QueueConfig>,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 10,
            poll_interval: 1,
            cron_interval: 60,
            heartbeat_interval: 10,
            auto_purge: Some(30 * 86400), // 30 days; set to none/"off" to disable
            priority_decay: 0,
            queues: HashMap::new(),
        }
    }
}

impl JobsConfig {
    /// The operator's per-queue `retries`, keyed by queue name.
    ///
    /// Only queues that set the key appear — a queue without one leaves
    /// [`JobDefinition::effective_retries`] to fall back. Every consumer that
    /// resolves a job's retry count (the scheduler, the gRPC service, the Lua
    /// job API) derives its map here, so they cannot disagree about which
    /// queues carry an override.
    ///
    /// [`JobDefinition::effective_retries`]: crate::core::job::JobDefinition::effective_retries
    #[must_use]
    pub fn queue_retries(&self) -> HashMap<String, u32> {
        self.queues
            .iter()
            .filter_map(|(name, q)| q.retries.map(|r| (name.clone(), r)))
            .collect()
    }

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
        // Why only `images` and `email`: these are the queues that
        // host framework-owned **system jobs**
        // (`_system_image_convert`, `_system_email`) — inserted by
        // Rust code with no `crap.jobs.define(...)` call. Without
        // queue-level defaults seeded here, those jobs would resolve
        // to `timeout = 0` and `retries = 0` on a fresh `crap.toml`.
        // User-defined Lua jobs carry their own `JobDefinition`
        // defaults; their queues never need seeding here.
        //
        // Per-field fill: operator-supplied `Some(value)` (including
        // `Some(0)` for "explicitly unlimited / no retries") wins;
        // only truly missing fields get the framework default. An
        // operator writing only `[jobs.queues.images] concurrency = 4`
        // KEEPS the framework's timeout and retries defaults, and so
        // on for the email queue.
        //
        // INVARIANT: every queue named by a system job
        // (`core::job::system::SYSTEM_JOB_SLUGS`) must get its three
        // fields seeded here. Adding a new system job means adding a
        // matching block below — `system_queues_have_all_defaults_seeded`
        // in the test module guards this.
        let images = self.queues.entry("images".to_string()).or_default();
        if images.concurrency.is_none() {
            images.concurrency = Some(DEFAULT_IMAGES_QUEUE_CONCURRENCY);
        }
        if images.timeout.is_none() {
            images.timeout = Some(DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS);
        }
        if images.retries.is_none() {
            images.retries = Some(DEFAULT_IMAGES_QUEUE_RETRIES);
        }

        let bulk = self
            .queues
            .entry(SYSTEM_BULK_QUEUE.to_string())
            .or_default();
        if bulk.concurrency.is_none() {
            bulk.concurrency = Some(DEFAULT_BULK_QUEUE_CONCURRENCY);
        }
        if bulk.timeout.is_none() {
            bulk.timeout = Some(DEFAULT_BULK_QUEUE_TIMEOUT_SECS);
        }
        if bulk.retries.is_none() {
            // ZERO retries by design: the batch op is atomic, but a crash in
            // the window between its commit and the completion mark would
            // make a retry re-apply the whole batch. Re-queue explicitly
            // instead.
            bulk.retries = Some(DEFAULT_BULK_QUEUE_RETRIES);
        }

        let email = self.queues.entry("email".to_string()).or_default();
        if email.concurrency.is_none() {
            email.concurrency = Some(DEFAULT_EMAIL_QUEUE_CONCURRENCY);
        }
        if email.timeout.is_none() {
            email.timeout = Some(DEFAULT_EMAIL_QUEUE_TIMEOUT_SECS);
        }
        if email.retries.is_none() {
            email.retries = Some(DEFAULT_EMAIL_QUEUE_RETRIES);
        }
    }

    /// This queue's configured timeout, or `fallback` when the queue has no
    /// entry or opted out of a per-queue timeout with `Some(0)` — the
    /// scheduler falls back to the same hard-coded default for system jobs in
    /// that queue, so the two views of "how long may this run take" agree.
    fn queue_timeout_or(&self, name: &str, fallback: u64) -> u64 {
        self.queues
            .get(name)
            .map(QueueConfig::effective_timeout)
            .filter(|t| *t > 0)
            .unwrap_or(fallback)
    }

    /// The longest a single job run can occupy a worker, per configuration.
    ///
    /// Every framework-owned queue contributes its configured timeout or the
    /// scheduler's hard-coded default for it, whichever is larger in effect;
    /// operator-declared queues contribute their configured timeout. A user
    /// job's own `crap.jobs.define({ timeout = N })` is NOT visible here —
    /// it lives in the registry, not in `crap.toml` — so a deployment whose
    /// Lua jobs declare longer timeouts than any queue should raise the
    /// matching `[jobs.queues.<name>] timeout` to keep a graceful stop from
    /// cutting those runs short.
    #[must_use]
    pub fn longest_job_timeout_secs(&self) -> u64 {
        [
            (IMAGE_CONVERT_QUEUE, DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS),
            (SYSTEM_EMAIL_QUEUE, DEFAULT_EMAIL_QUEUE_TIMEOUT_SECS),
            (SYSTEM_BULK_QUEUE, DEFAULT_BULK_QUEUE_TIMEOUT_SECS),
        ]
        .into_iter()
        .map(|(name, fallback)| self.queue_timeout_or(name, fallback))
        .chain(self.queues.values().map(QueueConfig::effective_timeout))
        .max()
        .unwrap_or(0)
    }

    /// How long a graceful stop waits for in-flight jobs before giving up:
    /// the longest a run can legitimately take plus the drain grace.
    ///
    /// `serve`, the standalone worker, and `work --stop` all derive their
    /// deadline from this one value. Killing a worker below it turns a
    /// graceful stop into a crash: the run dies with a fresh heartbeat (so
    /// stale recovery waits out the full heartbeat window before reclaiming
    /// it), a no-retry run like `_system_bulk` goes terminally stale, and any
    /// post-commit effect it had not reached yet is simply lost.
    #[must_use]
    pub fn drain_deadline_secs(&self) -> u64 {
        self.longest_job_timeout_secs()
            .saturating_add(JOB_DRAIN_GRACE_SECS)
    }

    /// Effective `max_attempts` for the `_system_image_convert`
    /// system job, derived from `[jobs.queues.images] retries` (+ 1
    /// for the initial attempt). Falls back to a hardcoded `3` if
    /// the queue has no entry — matches the framework default
    /// applied by `apply_queue_defaults` so behaviour is identical
    /// whether or not the defaults have been applied.
    #[must_use]
    pub fn system_image_max_attempts(&self) -> u32 {
        self.queues
            .get("images")
            .and_then(QueueConfig::effective_max_attempts)
            .unwrap_or(DEFAULT_IMAGES_QUEUE_RETRIES.saturating_add(1))
    }

    /// Effective `max_attempts` for the `_system_email` system job,
    /// derived from `[jobs.queues.email] retries` (+ 1 for the
    /// initial attempt). Falls back to the hardcoded framework
    /// default if the queue has no entry — matches what
    /// `apply_queue_defaults` would have applied, so behaviour is
    /// identical on call paths that don't run the load-time defaults
    /// (e.g. tests building a `JobsConfig` by hand).
    #[must_use]
    pub fn system_email_max_attempts(&self) -> u32 {
        self.queues
            .get("email")
            .and_then(QueueConfig::effective_max_attempts)
            .unwrap_or(DEFAULT_EMAIL_QUEUE_RETRIES.saturating_add(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;

    #[test]
    fn auto_purge_default_config() {
        let cfg = JobsConfig::default();
        assert_eq!(cfg.auto_purge, Some(30 * 86400));
    }

    #[test]
    fn jobs_config_defaults() {
        let cfg = JobsConfig::default();
        assert_eq!(cfg.max_concurrent, 10);
        assert_eq!(cfg.poll_interval, 1);
        assert_eq!(cfg.cron_interval, 60);
        assert_eq!(cfg.heartbeat_interval, 10);
        assert_eq!(cfg.auto_purge, Some(30 * 86400));
        // `queues` is empty in pure Default; `apply_queue_defaults` is
        // what seeds the framework defaults at load time.
        assert!(cfg.queues.is_empty());
    }

    #[test]
    fn apply_queue_defaults_seeds_images() {
        let mut cfg = JobsConfig::default();
        cfg.apply_queue_defaults();
        assert_eq!(
            cfg.queues["images"].concurrency,
            Some(DEFAULT_IMAGES_QUEUE_CONCURRENCY)
        );
        assert_eq!(
            cfg.queues["images"].timeout,
            Some(DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS)
        );
    }

    #[test]
    fn apply_queue_defaults_preserves_operator_concurrency_override() {
        let mut cfg = JobsConfig::default();
        cfg.queues.insert(
            "images".to_string(),
            QueueConfig {
                concurrency: Some(8),
                timeout: None,
                retries: None,
            },
        );
        cfg.apply_queue_defaults();
        // Operator concurrency wins, framework timeout default fills in.
        assert_eq!(cfg.queues["images"].concurrency, Some(8));
        assert_eq!(
            cfg.queues["images"].timeout,
            Some(DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS)
        );
    }

    #[test]
    fn apply_queue_defaults_preserves_operator_timeout_override() {
        let mut cfg = JobsConfig::default();
        cfg.queues.insert(
            "images".to_string(),
            QueueConfig {
                concurrency: None,
                timeout: Some(600),
                retries: None,
            },
        );
        cfg.apply_queue_defaults();
        // Operator timeout wins, framework concurrency default fills in.
        // This is the critical test: without `Option<T>`, an operator
        // tuning ONLY timeout would lose the concurrency cap and get
        // CPU saturation. With `Option<T>` they get safe defaults
        // for fields they didn't mention.
        assert_eq!(
            cfg.queues["images"].concurrency,
            Some(DEFAULT_IMAGES_QUEUE_CONCURRENCY)
        );
        assert_eq!(cfg.queues["images"].timeout, Some(600));
    }

    #[test]
    fn apply_queue_defaults_respects_explicit_unlimited() {
        let mut cfg = JobsConfig::default();
        cfg.queues.insert(
            "images".to_string(),
            QueueConfig {
                concurrency: Some(0), // operator explicitly chose unlimited
                timeout: None,
                retries: None,
            },
        );
        cfg.apply_queue_defaults();
        // `Some(0)` is "operator-chosen unlimited" — NOT silently
        // upgraded to the framework default.
        assert_eq!(cfg.queues["images"].concurrency, Some(0));
        assert_eq!(
            cfg.queues["images"].timeout,
            Some(DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS)
        );
    }

    #[test]
    fn apply_queue_defaults_preserves_operator_retries_override() {
        let mut cfg = JobsConfig::default();
        cfg.queues.insert(
            "images".to_string(),
            QueueConfig {
                concurrency: None,
                timeout: None,
                retries: Some(5),
            },
        );
        cfg.apply_queue_defaults();
        // Operator retries win; framework concurrency + timeout
        // defaults fill in the unspecified fields.
        assert_eq!(cfg.queues["images"].retries, Some(5));
        assert_eq!(
            cfg.queues["images"].concurrency,
            Some(DEFAULT_IMAGES_QUEUE_CONCURRENCY)
        );
        assert_eq!(
            cfg.queues["images"].timeout,
            Some(DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS)
        );
    }

    #[test]
    fn apply_queue_defaults_respects_explicit_zero_retries() {
        let mut cfg = JobsConfig::default();
        cfg.queues.insert(
            "images".to_string(),
            QueueConfig {
                concurrency: None,
                timeout: None,
                retries: Some(0), // operator chose: one attempt, no retries
            },
        );
        cfg.apply_queue_defaults();
        // `Some(0)` is "operator-chosen no retries" — NOT silently
        // upgraded to the framework default of 2.
        assert_eq!(cfg.queues["images"].retries, Some(0));
    }

    #[test]
    fn effective_max_attempts_translates_retries_to_attempts() {
        let q = QueueConfig {
            concurrency: None,
            timeout: None,
            retries: Some(2),
        };
        // retries + 1 = 3 total attempts
        assert_eq!(q.effective_max_attempts(), Some(3));

        let q_zero = QueueConfig {
            concurrency: None,
            timeout: None,
            retries: Some(0),
        };
        assert_eq!(q_zero.effective_max_attempts(), Some(1));

        let q_none = QueueConfig {
            concurrency: None,
            timeout: None,
            retries: None,
        };
        assert_eq!(q_none.effective_max_attempts(), None);
    }

    #[test]
    fn system_image_max_attempts_default_when_unset() {
        // Pure Default has empty queues — no `images` entry.
        let cfg = JobsConfig::default();
        // Falls back to DEFAULT_IMAGES_QUEUE_RETRIES + 1 = 3.
        assert_eq!(cfg.system_image_max_attempts(), 3);
    }

    #[test]
    fn system_image_max_attempts_uses_framework_default_after_apply() {
        let mut cfg = JobsConfig::default();
        cfg.apply_queue_defaults();
        // After apply_queue_defaults, images.retries = 2, so
        // max_attempts = 3. Must match the fallback for behaviour
        // continuity (the load path applies defaults; defensive paths
        // don't, but should observe the same number).
        assert_eq!(cfg.system_image_max_attempts(), 3);
    }

    #[test]
    fn system_email_max_attempts_default_when_unset() {
        let cfg = JobsConfig::default();
        // Falls back to DEFAULT_EMAIL_QUEUE_RETRIES + 1 = 4.
        assert_eq!(cfg.system_email_max_attempts(), 4);
    }

    #[test]
    fn system_email_max_attempts_uses_framework_default_after_apply() {
        let mut cfg = JobsConfig::default();
        cfg.apply_queue_defaults();
        assert_eq!(cfg.system_email_max_attempts(), 4);
    }

    #[test]
    fn system_email_max_attempts_honors_operator_override() {
        let mut cfg = JobsConfig::default();
        cfg.queues.insert(
            "email".to_string(),
            QueueConfig {
                concurrency: None,
                timeout: None,
                retries: Some(0), // operator: no retries, one attempt
            },
        );
        cfg.apply_queue_defaults();
        assert_eq!(cfg.system_email_max_attempts(), 1);
    }

    /// Pin the invariant from `apply_queue_defaults`: every queue
    /// that hosts a system job (slug starts with `_system_`) must have
    /// all three `QueueConfig` fields seeded after the load-time
    /// defaults run. Catches the regression where a new system job
    /// is added (e.g. `_system_retention`) but `apply_queue_defaults`
    /// is forgotten — the job would otherwise resolve to `timeout = 0`
    /// and `retries = 0` on a fresh config and silently misbehave.
    #[test]
    fn system_queues_have_all_defaults_seeded() {
        let mut cfg = JobsConfig::default();
        cfg.apply_queue_defaults();

        for queue in [SYSTEM_EMAIL_QUEUE, IMAGE_CONVERT_QUEUE, SYSTEM_BULK_QUEUE] {
            let q = cfg.queues.get(queue).unwrap_or_else(|| {
                panic!(
                    "system queue '{queue}' missing from apply_queue_defaults — \
                     add a seeding block to JobsConfig::apply_queue_defaults"
                )
            });
            assert!(
                q.concurrency.is_some(),
                "system queue '{queue}' missing concurrency default"
            );
            assert!(
                q.timeout.is_some(),
                "system queue '{queue}' missing timeout default"
            );
            assert!(
                q.retries.is_some(),
                "system queue '{queue}' missing retries default"
            );
        }
    }

    #[test]
    fn apply_queue_defaults_seeds_email() {
        let mut cfg = JobsConfig::default();
        cfg.apply_queue_defaults();
        assert_eq!(
            cfg.queues["email"].concurrency,
            Some(DEFAULT_EMAIL_QUEUE_CONCURRENCY)
        );
        assert_eq!(
            cfg.queues["email"].timeout,
            Some(DEFAULT_EMAIL_QUEUE_TIMEOUT_SECS)
        );
        assert_eq!(
            cfg.queues["email"].retries,
            Some(DEFAULT_EMAIL_QUEUE_RETRIES)
        );
    }

    #[test]
    fn system_image_max_attempts_honors_operator_override() {
        let mut cfg = JobsConfig::default();
        cfg.queues.insert(
            "images".to_string(),
            QueueConfig {
                concurrency: None,
                timeout: None,
                retries: Some(7),
            },
        );
        cfg.apply_queue_defaults();
        // 7 retries → 8 total attempts.
        assert_eq!(cfg.system_image_max_attempts(), 8);
    }

    #[test]
    fn apply_queue_defaults_preserves_other_queues() {
        let mut cfg = JobsConfig::default();
        cfg.queues.insert(
            "emails".to_string(),
            QueueConfig {
                concurrency: Some(4),
                timeout: None,
                retries: None,
            },
        );
        cfg.apply_queue_defaults();
        // emails has no framework default, so timeout stays None.
        assert_eq!(cfg.queues["emails"].concurrency, Some(4));
        assert_eq!(cfg.queues["emails"].timeout, None);
        // `images` default still applied.
        assert_eq!(
            cfg.queues["images"].concurrency,
            Some(DEFAULT_IMAGES_QUEUE_CONCURRENCY)
        );
    }

    #[test]
    fn loaded_config_has_images_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Empty TOML — operator set nothing.
        std::fs::write(tmp.path().join("crap.toml"), "").unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(
            config.jobs.queues["images"].concurrency,
            Some(DEFAULT_IMAGES_QUEUE_CONCURRENCY)
        );
        assert_eq!(
            config.jobs.queues["images"].timeout,
            Some(DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS)
        );
    }

    #[test]
    fn loaded_config_operator_partial_override_keeps_other_defaults() {
        // Operator writes ONLY concurrency. With `Option<T>`, missing
        // `timeout` deserializes to `None`, then `apply_queue_defaults`
        // fills in the framework timeout default. Operator-tuned
        // field and framework-tuned field coexist cleanly.
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs.queues.images]\nconcurrency = 4\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.queues["images"].concurrency, Some(4));
        assert_eq!(
            config.jobs.queues["images"].timeout,
            Some(DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS),
            "operator-supplied concurrency must not drop the framework's timeout default"
        );
    }

    #[test]
    fn queue_timeout_parses_minute_string() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs.queues.images]\nconcurrency = 2\ntimeout = \"10m\"\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.queues["images"].timeout, Some(600));
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
        let config = CrapConfig::load(tmp.path()).unwrap();
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
        let config = CrapConfig::load(tmp.path()).unwrap();
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
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.priority_decay, 3600);
    }

    // ── graceful-stop deadline ──────────────────────────────────────

    /// A bare `Default` has no `queues` entries at all, yet the scheduler
    /// still runs system jobs with its hard-coded timeouts — the deadline
    /// must cover the longest of those (bulk), not collapse to the grace.
    #[test]
    fn drain_deadline_covers_the_framework_defaults_without_queue_entries() {
        let cfg = JobsConfig::default();

        assert_eq!(
            cfg.longest_job_timeout_secs(),
            DEFAULT_BULK_QUEUE_TIMEOUT_SECS
        );
        assert_eq!(
            cfg.drain_deadline_secs(),
            DEFAULT_BULK_QUEUE_TIMEOUT_SECS + JOB_DRAIN_GRACE_SECS
        );
    }

    /// Seeding the load-time defaults must not move the number: the same
    /// worker behaviour has to yield the same stop deadline whether or not
    /// `apply_queue_defaults` has run (`work --stop` reads a loaded config,
    /// defensive paths may not).
    #[test]
    fn drain_deadline_is_unchanged_by_applying_queue_defaults() {
        let bare = JobsConfig::default().drain_deadline_secs();

        let mut seeded = JobsConfig::default();
        seeded.apply_queue_defaults();

        assert_eq!(seeded.drain_deadline_secs(), bare);
    }

    /// The drain deadline must cover the LONGEST outer timer the scheduler can
    /// arm, which for a self-limiting job (`_system_bulk`) is its own timeout
    /// plus [`SELF_LIMITING_JOB_GRACE_SECS`]. A shorter drain abandons a run
    /// that reached its cooperative deadline and is still rolling back and
    /// writing its terminal status — it dies with a fresh heartbeat, and a
    /// no-retry job like `_system_bulk` then goes terminally stale.
    #[test]
    fn drain_deadline_covers_the_self_limiting_outer_timer() {
        let mut configs = vec![JobsConfig::default()];

        let mut seeded = JobsConfig::default();
        seeded.apply_queue_defaults();
        configs.push(seeded);

        let mut short_bulk = JobsConfig::default();
        short_bulk.queues.insert(
            SYSTEM_BULK_QUEUE.to_string(),
            QueueConfig {
                concurrency: None,
                timeout: Some(30),
                retries: None,
            },
        );
        configs.push(short_bulk);

        let mut long_operator_queue = JobsConfig::default();
        long_operator_queue.queues.insert(
            "reports".to_string(),
            QueueConfig {
                concurrency: None,
                timeout: Some(7200),
                retries: None,
            },
        );
        configs.push(long_operator_queue);

        for cfg in configs {
            assert!(
                cfg.drain_deadline_secs()
                    >= cfg
                        .longest_job_timeout_secs()
                        .saturating_add(SELF_LIMITING_JOB_GRACE_SECS),
                "drain deadline {} must cover the self-limiting outer timer for {:?}",
                cfg.drain_deadline_secs(),
                cfg.queues
            );
        }
    }

    /// An operator queue that outlasts every system queue sets the deadline.
    #[test]
    fn drain_deadline_follows_the_longest_operator_timeout() {
        let mut cfg = JobsConfig::default();
        cfg.queues.insert(
            "reports".to_string(),
            QueueConfig {
                concurrency: None,
                timeout: Some(DEFAULT_BULK_QUEUE_TIMEOUT_SECS * 2),
                retries: None,
            },
        );

        assert_eq!(
            cfg.drain_deadline_secs(),
            DEFAULT_BULK_QUEUE_TIMEOUT_SECS * 2 + JOB_DRAIN_GRACE_SECS
        );
    }

    /// `timeout = 0` is "no per-queue timeout", which sends the scheduler to
    /// its hard-coded default for that system queue — so the deadline must
    /// keep covering that default rather than reading `0`.
    #[test]
    fn explicit_zero_timeout_still_counts_the_scheduler_fallback() {
        let mut cfg = JobsConfig::default();
        cfg.queues.insert(
            SYSTEM_BULK_QUEUE.to_string(),
            QueueConfig {
                concurrency: None,
                timeout: Some(0),
                retries: None,
            },
        );

        assert_eq!(
            cfg.longest_job_timeout_secs(),
            DEFAULT_BULK_QUEUE_TIMEOUT_SECS
        );
    }

    /// A shortened bulk timeout genuinely shortens the wait — the deadline
    /// tracks configuration instead of pinning the framework maximum.
    #[test]
    fn a_shorter_configured_bulk_timeout_shortens_the_deadline() {
        let mut cfg = JobsConfig::default();
        cfg.queues.insert(
            SYSTEM_BULK_QUEUE.to_string(),
            QueueConfig {
                concurrency: None,
                timeout: Some(120),
                retries: None,
            },
        );

        // `images` (300s) is now the longest configured run.
        assert_eq!(
            cfg.drain_deadline_secs(),
            DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS + JOB_DRAIN_GRACE_SECS
        );
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
        let config = CrapConfig::load(tmp.path()).unwrap();
        // 3 operator-declared queues + the framework-seeded `email` and
        // `bulk` (the operator's `images` overrides what
        // apply_queue_defaults would have seeded, so it adds nothing).
        assert_eq!(config.jobs.queues.len(), 5);
        assert_eq!(config.jobs.queues["default"].concurrency, Some(10));
        assert_eq!(config.jobs.queues["emails"].concurrency, Some(4));
        assert_eq!(config.jobs.queues["images"].concurrency, Some(2));
        assert_eq!(
            config.jobs.queues["email"].concurrency,
            Some(DEFAULT_EMAIL_QUEUE_CONCURRENCY)
        );
    }

    #[test]
    fn queues_parses_block_form() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs.queues.emails]\nconcurrency = 4\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.queues["emails"].concurrency, Some(4));
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
        let err = CrapConfig::load(tmp.path()).unwrap_err();
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
        let err = CrapConfig::load(tmp.path()).unwrap_err();
        // Walk the anyhow chain; the bogus-field message is on the
        // serde-level inner error.
        let chain = format!("{err:#}");
        assert!(
            chain.contains("bogus_field"),
            "expected unknown-field error mentioning 'bogus_field'; got chain: {chain}"
        );
    }
}
