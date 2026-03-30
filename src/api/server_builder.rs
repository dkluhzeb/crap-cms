//! Builder for [`GrpcStartParams`].

use std::{path::PathBuf, sync::Arc};

use crate::{
    api::server::GrpcStartParams,
    config::CrapConfig,
    core::{JwtSecret, Registry, event::EventBus, rate_limit::LoginRateLimiter},
    db::DbPool,
    hooks::HookRunner,
};

/// Builder for [`GrpcStartParams`]. Created via [`GrpcStartParams::builder`].
pub struct GrpcStartParamsBuilder {
    pool: Option<DbPool>,
    registry: Option<Arc<Registry>>,
    hook_runner: Option<HookRunner>,
    jwt_secret: Option<JwtSecret>,
    config: Option<CrapConfig>,
    config_dir: Option<PathBuf>,
    event_bus: Option<EventBus>,
    login_limiter: Option<Arc<LoginRateLimiter>>,
    ip_login_limiter: Option<Arc<LoginRateLimiter>>,
    forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    ip_forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
}

impl GrpcStartParamsBuilder {
    pub(crate) fn new() -> Self {
        Self {
            pool: None,
            registry: None,
            hook_runner: None,
            jwt_secret: None,
            config: None,
            config_dir: None,
            event_bus: None,
            login_limiter: None,
            ip_login_limiter: None,
            forgot_password_limiter: None,
            ip_forgot_password_limiter: None,
        }
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

    pub fn config(mut self, config: CrapConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub fn config_dir(mut self, config_dir: PathBuf) -> Self {
        self.config_dir = Some(config_dir);
        self
    }

    pub fn event_bus(mut self, event_bus: Option<EventBus>) -> Self {
        self.event_bus = event_bus;
        self
    }

    pub fn login_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.login_limiter = Some(limiter);
        self
    }

    pub fn ip_login_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.ip_login_limiter = Some(limiter);
        self
    }

    pub fn forgot_password_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.forgot_password_limiter = Some(limiter);
        self
    }

    pub fn ip_forgot_password_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.ip_forgot_password_limiter = Some(limiter);
        self
    }

    pub fn build(self) -> GrpcStartParams {
        GrpcStartParams {
            pool: self.pool.expect("pool is required"),
            registry: self.registry.expect("registry is required"),
            hook_runner: self.hook_runner.expect("hook_runner is required"),
            jwt_secret: self.jwt_secret.expect("jwt_secret is required"),
            config: self.config.expect("config is required"),
            config_dir: self.config_dir.expect("config_dir is required"),
            event_bus: self.event_bus,
            login_limiter: self.login_limiter.expect("login_limiter is required"),
            ip_login_limiter: self.ip_login_limiter.expect("ip_login_limiter is required"),
            forgot_password_limiter: self
                .forgot_password_limiter
                .expect("forgot_password_limiter is required"),
            ip_forgot_password_limiter: self
                .ip_forgot_password_limiter
                .expect("ip_forgot_password_limiter is required"),
        }
    }
}
