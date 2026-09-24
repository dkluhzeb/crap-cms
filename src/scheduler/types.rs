//! Scheduler types -- parameters and internal config structs.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    config::JobsConfig,
    core::{SharedEmailProvider, SharedStorage},
    hooks::LuaCrudInfra,
    service::AppInfra,
};

/// Parameters for starting the scheduler, built via
/// [`SchedulerParams::builder`]. The four fields every caller supplies are
/// the builder's arguments; the rest carry the `serve` defaults — every
/// queue, cron on — so only `crap-cms work` has to name them.
///
/// The scheduler consumes the "core" subset of [`AppInfra`] (pool, hook
/// runner, registry, storage, locale config); `serve` shares the boot bundle,
/// the standalone `work` command assembles one via `AppInfra::standalone`.
///
/// Email-job timeout / retries / concurrency are NOT here — they flow
/// through `JobsConfig::queues["email"]` (resolved by
/// `apply_queue_defaults` at load time, same path as image jobs).
pub struct SchedulerParams {
    pub infra: Arc<AppInfra>,
    pub config: JobsConfig,
    /// The database waits a heartbeat write can legitimately sit in; the
    /// stale-job threshold is derived from them.
    pub db_timeouts: DbTimeouts,
    pub shutdown: CancellationToken,
    pub email_provider: Option<SharedEmailProvider>,
    /// The queues this process claims from (`crap-cms work --queues a,b`).
    /// `None` — the default, and what `serve` always passes — claims from
    /// every queue. The filter is applied inside the claim query, so a
    /// filtered worker never takes a run its peers are meant to handle.
    pub queues: Option<Vec<String>>,
    /// Whether this process evaluates cron schedules. `false` is
    /// `crap-cms work --no-cron`: the worker still executes jobs and still
    /// runs the retention purges, it just leaves the schedule evaluation to
    /// its peers. Defaults to `true`.
    pub run_cron: bool,
}

impl SchedulerParams {
    /// Start a builder from the fields no caller can default.
    #[must_use]
    pub fn builder(
        infra: Arc<AppInfra>,
        config: JobsConfig,
        db_timeouts: DbTimeouts,
        shutdown: CancellationToken,
    ) -> SchedulerParamsBuilder {
        SchedulerParamsBuilder {
            infra,
            config,
            db_timeouts,
            shutdown,
            email_provider: None,
            queues: None,
            run_cron: true,
        }
    }
}

/// Builder for [`SchedulerParams`]; see [`SchedulerParams::builder`].
pub struct SchedulerParamsBuilder {
    infra: Arc<AppInfra>,
    config: JobsConfig,
    db_timeouts: DbTimeouts,
    shutdown: CancellationToken,
    email_provider: Option<SharedEmailProvider>,
    queues: Option<Vec<String>>,
    run_cron: bool,
}

impl SchedulerParamsBuilder {
    /// The provider `_system_email` jobs send through. Without one those jobs
    /// fail rather than silently vanish.
    #[must_use]
    pub fn email_provider(mut self, provider: Option<SharedEmailProvider>) -> Self {
        self.email_provider = provider;

        self
    }

    /// Restrict this process to a set of queues; `None` keeps every queue.
    #[must_use]
    pub fn queues(mut self, queues: Option<Vec<String>>) -> Self {
        self.queues = queues;

        self
    }

    /// Whether this process evaluates cron schedules.
    #[must_use]
    pub fn run_cron(mut self, run_cron: bool) -> Self {
        self.run_cron = run_cron;

        self
    }

    #[must_use]
    pub fn build(self) -> SchedulerParams {
        SchedulerParams {
            infra: self.infra,
            config: self.config,
            db_timeouts: self.db_timeouts,
            shutdown: self.shutdown,
            email_provider: self.email_provider,
            queues: self.queues,
            run_cron: self.run_cron,
        }
    }
}

/// How long one database write may wait before it runs: for a connection
/// from the write pool (`[database] connection_timeout`), then for the
/// engine's write lock (`[database] busy_timeout`, `SQLite`).
#[derive(Clone, Copy, Debug)]
pub struct DbTimeouts {
    pub busy_timeout_ms: u64,
    pub connection_timeout_secs: u64,
}

impl DbTimeouts {
    #[must_use]
    pub fn new(busy_timeout_ms: u64, connection_timeout_secs: u64) -> Self {
        Self {
            busy_timeout_ms,
            connection_timeout_secs,
        }
    }
}

/// One run this process is executing: its id and the attempt it claimed.
///
/// The attempt is what the heartbeat's compare-and-set matches on, so a run
/// that outlived its heartbeat window and was reclaimed — possibly claimed
/// again as a later attempt — can never keep that later attempt's heartbeat
/// fresh.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RunningJob {
    pub id: String,
    pub attempt: u32,
}

impl RunningJob {
    pub(super) fn new(id: String, attempt: u32) -> Self {
        Self { id, attempt }
    }
}

/// The runs this process is executing: added when a run is claimed, removed
/// once it has ended, refreshed by every heartbeat, and listed by the
/// shutdown drain.
pub(super) type RunningJobs = Arc<Mutex<Vec<RunningJob>>>;

/// Job-execution config shared by every poll — the parts the poll reads
/// from `JobsConfig` (image conversion concurrency, priority-decay aging,
/// per-queue timeouts) plus the execution infrastructure the spawned
/// jobs need (storage for system image jobs, the Lua-CRUD infra for
/// user handlers). Future system jobs (email retention sweeps etc.)
/// land here.
pub(super) struct TickJobConfig {
    /// Full infra bundle for system jobs that execute service ops
    /// (`_system_bulk`).
    pub app_infra: Arc<AppInfra>,
    pub priority_decay: u64,
    /// Per-queue aggregate concurrency caps, sourced from
    /// `[jobs.queues.<name>] concurrency = N` plus framework defaults
    /// applied by `JobsConfig::apply_queue_defaults` (currently just
    /// `images = { concurrency = 2 }`). Operator overrides win;
    /// queues without entries are unconstrained beyond the global
    /// `max_concurrent` and per-slug caps.
    pub queue_concurrency: HashMap<String, u32>,
    /// Per-queue timeouts in seconds, sourced from
    /// `[jobs.queues.<name>] timeout = "..."`. Used by
    /// `resolve_job_def` for system jobs that have no
    /// `JobDefinition::timeout`; user jobs keep their declared
    /// per-job timeout. Queues without an entry fall back to a
    /// hardcoded default in the scheduler.
    pub queue_timeouts: HashMap<String, u64>,
    /// The queues this worker claims from (`crap-cms work --queues a,b`), or
    /// `None` for every queue. Handed straight to the claim query, so a
    /// filtered worker's poll never even sees a run outside its queues.
    pub queues: Option<Arc<[String]>>,
    pub storage: SharedStorage,
    /// Event transport, populate cache and email context threaded into user
    /// job handlers' Lua CRUD calls (cloned per handler; `run_job_handler`
    /// injects and flushes the event queue per invocation). Built from the
    /// scheduler's [`AppInfra`] so job writes publish live-update events,
    /// invalidate the populate cache and issue account verifications like
    /// every other surface.
    pub lua_infra: LuaCrudInfra,
    /// Tracker every job task is spawned on, so a shutdown can wait for the
    /// runs already in flight instead of dropping them mid-transaction.
    /// Cloning is cheap (the tracker is internally shared).
    pub job_tasks: TaskTracker,
}
