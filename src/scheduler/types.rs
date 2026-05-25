//! Scheduler types -- parameters and internal config structs.

use std::{collections::HashMap, sync::Arc};

use tokio_util::sync::CancellationToken;

use crate::{
    config::{JobsConfig, LocaleConfig},
    core::{Registry, SharedEmailProvider, SharedStorage},
    db::DbPool,
    hooks::HookRunner,
};

/// Parameters for starting the scheduler. Constructed via plain
/// struct literal at the call site -- both callers (`crap-cms work`
/// and `serve`'s startup) supply every field, so a builder added no
/// real DX over the literal form.
///
/// Email-job timeout / retries / concurrency are NOT here — they flow
/// through `JobsConfig::queues["email"]` (resolved by
/// `apply_queue_defaults` at load time, same path as image jobs).
pub struct SchedulerParams {
    pub pool: DbPool,
    pub hook_runner: HookRunner,
    pub registry: Arc<Registry>,
    pub config: JobsConfig,
    pub shutdown: CancellationToken,
    pub storage: SharedStorage,
    pub locale_config: LocaleConfig,
    pub email_provider: Option<SharedEmailProvider>,
}

/// Per-tick system-job config — the parts the poll loop reads from
/// `JobsConfig` but doesn't carry on `JobDefinition`. Currently:
/// image conversion concurrency + priority-decay aging. Future system
/// jobs (email retention sweeps etc.) land here.
pub(super) struct SystemJobConfig {
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
}
