//! Scheduler types — parameters, builder, and internal config structs.

use crate::config::LocaleConfig;
use crate::{
    config::JobsConfig,
    core::{SharedRegistry, email::SharedEmailProvider, upload::SharedStorage},
    db::DbPool,
    hooks::HookRunner,
};
use tokio_util::sync::CancellationToken;

/// Parameters for starting the scheduler.
pub struct SchedulerParams {
    pub(super) pool: DbPool,
    pub(super) hook_runner: HookRunner,
    pub(super) registry: SharedRegistry,
    pub(super) config: JobsConfig,
    pub(super) shutdown: CancellationToken,
    pub(super) storage: SharedStorage,
    pub(super) locale_config: LocaleConfig,
    pub(super) email_provider: Option<SharedEmailProvider>,
    pub(super) email_queue_timeout: u64,
    pub(super) email_queue_concurrency: u32,
}

/// Builder for [`SchedulerParams`].
pub struct SchedulerParamsBuilder {
    pool: DbPool,
    hook_runner: HookRunner,
    registry: SharedRegistry,
    config: JobsConfig,
    shutdown: CancellationToken,
    storage: SharedStorage,
    locale_config: LocaleConfig,
    email_provider: Option<SharedEmailProvider>,
    email_queue_timeout: u64,
    email_queue_concurrency: u32,
}

impl SchedulerParamsBuilder {
    /// Create a new builder with required parameters.
    pub fn new(
        pool: DbPool,
        hook_runner: HookRunner,
        registry: SharedRegistry,
        config: JobsConfig,
        shutdown: CancellationToken,
        storage: SharedStorage,
        locale_config: LocaleConfig,
    ) -> Self {
        Self {
            pool,
            hook_runner,
            registry,
            config,
            shutdown,
            storage,
            locale_config,
            email_provider: None,
            email_queue_timeout: 30,
            email_queue_concurrency: 5,
        }
    }

    /// Set the email provider for system email jobs.
    pub fn email_provider(mut self, provider: SharedEmailProvider) -> Self {
        self.email_provider = Some(provider);
        self
    }

    /// Set the timeout for email queue processing.
    pub fn email_queue_timeout(mut self, timeout: u64) -> Self {
        self.email_queue_timeout = timeout;
        self
    }

    /// Set the concurrency limit for email queue processing.
    pub fn email_queue_concurrency(mut self, concurrency: u32) -> Self {
        self.email_queue_concurrency = concurrency;
        self
    }

    /// Build the scheduler parameters.
    pub fn build(self) -> SchedulerParams {
        SchedulerParams {
            pool: self.pool,
            hook_runner: self.hook_runner,
            registry: self.registry,
            config: self.config,
            shutdown: self.shutdown,
            storage: self.storage,
            locale_config: self.locale_config,
            email_provider: self.email_provider,
            email_queue_timeout: self.email_queue_timeout,
            email_queue_concurrency: self.email_queue_concurrency,
        }
    }
}

/// Email queue config passed to the poll loop.
pub(super) struct EmailQueueConfig {
    pub provider: Option<SharedEmailProvider>,
    pub timeout: u64,
    pub concurrency: u32,
}
