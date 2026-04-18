//! Builder for [`AdminStartParams`].

use std::{path::PathBuf, sync::Arc};

use crate::{
    admin::server::AdminStartParams,
    config::CrapConfig,
    core::{
        JwtSecret, Registry,
        auth::{SharedPasswordProvider, SharedTokenProvider},
        cache::SharedCache,
        event::{SharedEventTransport, SharedInvalidationTransport},
        rate_limit::LoginRateLimiter,
        upload::SharedStorage,
    },
    db::DbPool,
    hooks::HookRunner,
};

/// Builder for [`AdminStartParams`]. Created via [`AdminStartParams::builder`].
pub struct AdminStartParamsBuilder {
    config: Option<CrapConfig>,
    config_dir: Option<PathBuf>,
    pool: Option<DbPool>,
    registry: Option<Arc<Registry>>,
    hook_runner: Option<HookRunner>,
    jwt_secret: Option<JwtSecret>,
    event_transport: Option<SharedEventTransport>,
    login_limiter: Option<Arc<LoginRateLimiter>>,
    ip_login_limiter: Option<Arc<LoginRateLimiter>>,
    forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    ip_forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    storage: Option<SharedStorage>,
    token_provider: Option<SharedTokenProvider>,
    password_provider: Option<SharedPasswordProvider>,
    invalidation_transport: Option<SharedInvalidationTransport>,
    cache: Option<SharedCache>,
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
            event_transport: None,
            login_limiter: None,
            ip_login_limiter: None,
            forgot_password_limiter: None,
            ip_forgot_password_limiter: None,
            storage: None,
            token_provider: None,
            password_provider: None,
            invalidation_transport: None,
            cache: None,
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

    pub fn event_transport(mut self, transport: Option<SharedEventTransport>) -> Self {
        self.event_transport = transport;

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

    pub fn storage(mut self, storage: SharedStorage) -> Self {
        self.storage = Some(storage);

        self
    }

    pub fn token_provider(mut self, token_provider: SharedTokenProvider) -> Self {
        self.token_provider = Some(token_provider);

        self
    }

    pub fn password_provider(mut self, password_provider: SharedPasswordProvider) -> Self {
        self.password_provider = Some(password_provider);

        self
    }

    pub fn invalidation_transport(mut self, transport: SharedInvalidationTransport) -> Self {
        self.invalidation_transport = Some(transport);

        self
    }

    pub fn cache(mut self, cache: Option<SharedCache>) -> Self {
        self.cache = cache;

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
            event_transport: self.event_transport,
            login_limiter: self.login_limiter.expect("login_limiter is required"),
            ip_login_limiter: self.ip_login_limiter.expect("ip_login_limiter is required"),
            forgot_password_limiter: self
                .forgot_password_limiter
                .expect("forgot_password_limiter is required"),
            ip_forgot_password_limiter: self
                .ip_forgot_password_limiter
                .expect("ip_forgot_password_limiter is required"),
            storage: self.storage.expect("storage is required"),
            token_provider: self.token_provider.expect("token_provider is required"),
            password_provider: self
                .password_provider
                .expect("password_provider is required"),
            invalidation_transport: self.invalidation_transport,
            cache: self.cache,
        }
    }
}
