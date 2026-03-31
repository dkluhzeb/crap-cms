//! Builder for [`ContentServiceDeps`].

use std::{path::PathBuf, sync::Arc};

use crate::{
    api::service::ContentServiceDeps,
    config::CrapConfig,
    core::{
        JwtSecret, Registry, email::EmailRenderer, event::EventBus, rate_limit::LoginRateLimiter,
        upload::SharedStorage,
    },
    db::DbPool,
    hooks::HookRunner,
};

/// Builder for [`ContentServiceDeps`]. Created via [`ContentServiceDeps::builder`].
pub struct ContentServiceDepsBuilder {
    pool: Option<DbPool>,
    registry: Option<Arc<Registry>>,
    hook_runner: Option<HookRunner>,
    jwt_secret: Option<JwtSecret>,
    config: Option<CrapConfig>,
    config_dir: Option<PathBuf>,
    email_renderer: Option<Arc<EmailRenderer>>,
    event_bus: Option<EventBus>,
    login_limiter: Option<Arc<LoginRateLimiter>>,
    ip_login_limiter: Option<Arc<LoginRateLimiter>>,
    forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    ip_forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    storage: Option<SharedStorage>,
}

impl ContentServiceDepsBuilder {
    pub(crate) fn new() -> Self {
        Self {
            pool: None,
            registry: None,
            hook_runner: None,
            jwt_secret: None,
            config: None,
            config_dir: None,
            email_renderer: None,
            event_bus: None,
            login_limiter: None,
            ip_login_limiter: None,
            forgot_password_limiter: None,
            ip_forgot_password_limiter: None,
            storage: None,
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

    pub fn email_renderer(mut self, email_renderer: Arc<EmailRenderer>) -> Self {
        self.email_renderer = Some(email_renderer);
        self
    }

    pub fn event_bus(mut self, event_bus: Option<EventBus>) -> Self {
        self.event_bus = event_bus;
        self
    }

    pub fn login_limiter(mut self, login_limiter: Arc<LoginRateLimiter>) -> Self {
        self.login_limiter = Some(login_limiter);
        self
    }

    pub fn ip_login_limiter(mut self, ip_login_limiter: Arc<LoginRateLimiter>) -> Self {
        self.ip_login_limiter = Some(ip_login_limiter);
        self
    }

    pub fn forgot_password_limiter(
        mut self,
        forgot_password_limiter: Arc<LoginRateLimiter>,
    ) -> Self {
        self.forgot_password_limiter = Some(forgot_password_limiter);
        self
    }

    pub fn ip_forgot_password_limiter(
        mut self,
        ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    ) -> Self {
        self.ip_forgot_password_limiter = Some(ip_forgot_password_limiter);
        self
    }

    pub fn storage(mut self, storage: SharedStorage) -> Self {
        self.storage = Some(storage);
        self
    }

    pub fn build(self) -> ContentServiceDeps {
        ContentServiceDeps {
            pool: self.pool.expect("pool is required"),
            registry: self.registry.expect("registry is required"),
            hook_runner: self.hook_runner.expect("hook_runner is required"),
            jwt_secret: self.jwt_secret.expect("jwt_secret is required"),
            config: self.config.expect("config is required"),
            config_dir: self.config_dir.expect("config_dir is required"),
            email_renderer: self.email_renderer.expect("email_renderer is required"),
            event_bus: self.event_bus,
            login_limiter: self.login_limiter.expect("login_limiter is required"),
            ip_login_limiter: self.ip_login_limiter.expect("ip_login_limiter is required"),
            forgot_password_limiter: self
                .forgot_password_limiter
                .expect("forgot_password_limiter is required"),
            ip_forgot_password_limiter: self
                .ip_forgot_password_limiter
                .expect("ip_forgot_password_limiter is required"),
            storage: self.storage.expect("storage is required"),
        }
    }
}
