//! Builder for [`AdminStartParams`].

use std::{path::PathBuf, sync::Arc};

use crate::{
    config::CrapConfig,
    core::{JwtSecret, Registry, event::EventBus},
    db::DbPool,
    hooks::HookRunner,
};

use crate::admin::server::AdminStartParams;

/// Builder for [`AdminStartParams`]. Created via [`AdminStartParams::builder`].
pub struct AdminStartParamsBuilder {
    config: Option<CrapConfig>,
    config_dir: Option<PathBuf>,
    pool: Option<DbPool>,
    registry: Option<Arc<Registry>>,
    hook_runner: Option<HookRunner>,
    jwt_secret: Option<JwtSecret>,
    event_bus: Option<EventBus>,
}

impl AdminStartParamsBuilder {
    pub(crate) fn new() -> Self {
        Self {
            config: None,
            config_dir: None,
            pool: None,
            registry: None,
            hook_runner: None,
            jwt_secret: None,
            event_bus: None,
        }
    }

    pub fn config(mut self, config: CrapConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub fn config_dir(mut self, config_dir: PathBuf) -> Self {
        self.config_dir = Some(config_dir);
        self
    }

    pub fn pool(mut self, pool: DbPool) -> Self {
        self.pool = Some(pool);
        self
    }

    pub fn registry(mut self, registry: Arc<Registry>) -> Self {
        self.registry = Some(registry);
        self
    }

    pub fn hook_runner(mut self, hook_runner: HookRunner) -> Self {
        self.hook_runner = Some(hook_runner);
        self
    }

    pub fn jwt_secret(mut self, jwt_secret: impl Into<JwtSecret>) -> Self {
        self.jwt_secret = Some(jwt_secret.into());
        self
    }

    pub fn event_bus(mut self, event_bus: Option<EventBus>) -> Self {
        self.event_bus = event_bus;
        self
    }

    pub fn build(self) -> AdminStartParams {
        AdminStartParams {
            config: self.config.expect("config is required"),
            config_dir: self.config_dir.expect("config_dir is required"),
            pool: self.pool.expect("pool is required"),
            registry: self.registry.expect("registry is required"),
            hook_runner: self.hook_runner.expect("hook_runner is required"),
            jwt_secret: self.jwt_secret.expect("jwt_secret is required"),
            event_bus: self.event_bus,
        }
    }
}
