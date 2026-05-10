//! Scheduler types -- parameters and internal config structs.

use crate::config::LocaleConfig;
use crate::{
    config::JobsConfig,
    core::{SharedRegistry, email::SharedEmailProvider, upload::SharedStorage},
    db::DbPool,
    hooks::HookRunner,
};
use tokio_util::sync::CancellationToken;

/// Parameters for starting the scheduler. Constructed via plain
/// struct literal at the call site -- both callers (`crap-cms work`
/// and `serve`'s startup) supply every field, so a builder added no
/// real DX over the literal form.
pub struct SchedulerParams {
    pub pool: DbPool,
    pub hook_runner: HookRunner,
    pub registry: SharedRegistry,
    pub config: JobsConfig,
    pub shutdown: CancellationToken,
    pub storage: SharedStorage,
    pub locale_config: LocaleConfig,
    pub email_provider: Option<SharedEmailProvider>,
    pub email_queue_timeout: u64,
    pub email_queue_concurrency: u32,
}

/// Email queue config passed to the poll loop.
pub(super) struct EmailQueueConfig {
    pub provider: Option<SharedEmailProvider>,
    pub timeout: u64,
    pub concurrency: u32,
}
