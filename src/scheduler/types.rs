//! Scheduler types -- parameters and internal config structs.

use std::{collections::HashMap, sync::Arc};

use tokio_util::sync::CancellationToken;

use crate::{
    config::JobsConfig,
    core::{SharedEmailProvider, SharedStorage},
    hooks::LuaCrudInfra,
    service::AppInfra,
};

/// Parameters for starting the scheduler. Constructed via plain
/// struct literal at the call site -- both callers (`crap-cms work`
/// and `serve`'s startup) supply every field, so a builder added no
/// real DX over the literal form.
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
    pub shutdown: CancellationToken,
    pub email_provider: Option<SharedEmailProvider>,
}

/// Per-tick job-execution config — the parts the poll loop reads from
/// `JobsConfig` (image conversion concurrency, priority-decay aging,
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
    pub storage: SharedStorage,
    /// Event transport + populate cache threaded into user job handlers'
    /// Lua CRUD calls (cloned per handler; `run_job_handler` injects and
    /// flushes the event queue per invocation). Built from the scheduler's
    /// [`AppInfra`] so job writes publish live-update events and invalidate
    /// the populate cache like every other surface.
    pub lua_infra: LuaCrudInfra,
}
