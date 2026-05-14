//! [`ContentServiceDeps`] — the dependency bundle handed to
//! [`ContentService::new`](super::ContentService::new) — and its builder.

use std::{path::PathBuf, sync::Arc};

use crate::{
    config::CrapConfig,
    core::{
        Registry, SharedCache, SharedEventTransport, SharedInvalidationTransport,
        SharedPasswordProvider, SharedStorage, SharedTokenProvider, email::EmailRenderer,
        rate_limit::LoginRateLimiter,
    },
    db::{DbPool, query::SharedPopulateSingleflight},
    hooks::HookRunner,
};

/// Dependencies for constructing a `ContentService`.
pub struct ContentServiceDeps {
    pub pool: DbPool,
    pub registry: Arc<Registry>,
    pub hook_runner: HookRunner,
    pub config: CrapConfig,
    pub config_dir: PathBuf,
    pub email_renderer: Arc<EmailRenderer>,
    pub event_transport: Option<SharedEventTransport>,
    pub login_limiter: Arc<LoginRateLimiter>,
    pub ip_login_limiter: Arc<LoginRateLimiter>,
    pub forgot_password_limiter: Arc<LoginRateLimiter>,
    pub ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    pub storage: SharedStorage,
    pub cache: SharedCache,
    pub token_provider: SharedTokenProvider,
    pub password_provider: SharedPasswordProvider,
    /// Optional: shared invalidation transport. When `None`, a fresh
    /// in-process one is created internally.
    pub invalidation_transport: Option<SharedInvalidationTransport>,
    /// Optional: shared populate singleflight. When `None`, a fresh
    /// process-wide one is created internally for this service.
    pub populate_singleflight: Option<SharedPopulateSingleflight>,
}

impl ContentServiceDeps {
    /// Create a builder for `ContentServiceDeps`.
    #[must_use]
    pub fn builder() -> ContentServiceDepsBuilder {
        ContentServiceDepsBuilder::new()
    }
}

/// Builder for [`ContentServiceDeps`]. Created via [`ContentServiceDeps::builder`].
pub struct ContentServiceDepsBuilder {
    pool: Option<DbPool>,
    registry: Option<Arc<Registry>>,
    hook_runner: Option<HookRunner>,
    config: Option<CrapConfig>,
    config_dir: Option<PathBuf>,
    email_renderer: Option<Arc<EmailRenderer>>,
    event_transport: Option<SharedEventTransport>,
    login_limiter: Option<Arc<LoginRateLimiter>>,
    ip_login_limiter: Option<Arc<LoginRateLimiter>>,
    forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    ip_forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    storage: Option<SharedStorage>,
    cache: Option<SharedCache>,
    token_provider: Option<SharedTokenProvider>,
    password_provider: Option<SharedPasswordProvider>,
    invalidation_transport: Option<SharedInvalidationTransport>,
    populate_singleflight: Option<SharedPopulateSingleflight>,
}

impl ContentServiceDepsBuilder {
    pub(crate) fn new() -> Self {
        Self {
            pool: None,
            registry: None,
            hook_runner: None,
            config: None,
            config_dir: None,
            email_renderer: None,
            event_transport: None,
            login_limiter: None,
            ip_login_limiter: None,
            forgot_password_limiter: None,
            ip_forgot_password_limiter: None,
            storage: None,
            cache: None,
            token_provider: None,
            password_provider: None,
            invalidation_transport: None,
            populate_singleflight: None,
        }
    }

    #[must_use]
    pub fn pool(mut self, pool: DbPool) -> Self {
        self.pool = Some(pool);

        self
    }

    #[must_use]
    pub fn registry(mut self, registry: Arc<Registry>) -> Self {
        self.registry = Some(registry);

        self
    }

    #[must_use]
    pub fn hook_runner(mut self, hook_runner: HookRunner) -> Self {
        self.hook_runner = Some(hook_runner);

        self
    }

    #[must_use]
    pub fn config(mut self, config: CrapConfig) -> Self {
        self.config = Some(config);

        self
    }

    #[must_use]
    pub fn config_dir(mut self, config_dir: PathBuf) -> Self {
        self.config_dir = Some(config_dir);

        self
    }

    #[must_use]
    pub fn email_renderer(mut self, email_renderer: Arc<EmailRenderer>) -> Self {
        self.email_renderer = Some(email_renderer);

        self
    }

    #[must_use]
    pub fn event_transport(mut self, transport: Option<SharedEventTransport>) -> Self {
        self.event_transport = transport;

        self
    }

    #[must_use]
    pub fn login_limiter(mut self, login_limiter: Arc<LoginRateLimiter>) -> Self {
        self.login_limiter = Some(login_limiter);

        self
    }

    #[must_use]
    pub fn ip_login_limiter(mut self, ip_login_limiter: Arc<LoginRateLimiter>) -> Self {
        self.ip_login_limiter = Some(ip_login_limiter);

        self
    }

    #[must_use]
    pub fn forgot_password_limiter(
        mut self,
        forgot_password_limiter: Arc<LoginRateLimiter>,
    ) -> Self {
        self.forgot_password_limiter = Some(forgot_password_limiter);

        self
    }

    #[must_use]
    pub fn ip_forgot_password_limiter(
        mut self,
        ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    ) -> Self {
        self.ip_forgot_password_limiter = Some(ip_forgot_password_limiter);

        self
    }

    #[must_use]
    pub fn storage(mut self, storage: SharedStorage) -> Self {
        self.storage = Some(storage);

        self
    }

    #[must_use]
    pub fn cache(mut self, cache: SharedCache) -> Self {
        self.cache = Some(cache);

        self
    }

    #[must_use]
    pub fn token_provider(mut self, token_provider: SharedTokenProvider) -> Self {
        self.token_provider = Some(token_provider);

        self
    }

    #[must_use]
    pub fn password_provider(mut self, password_provider: SharedPasswordProvider) -> Self {
        self.password_provider = Some(password_provider);

        self
    }

    #[must_use]
    pub fn invalidation_transport(mut self, transport: SharedInvalidationTransport) -> Self {
        self.invalidation_transport = Some(transport);

        self
    }

    #[must_use]
    pub fn populate_singleflight(mut self, singleflight: SharedPopulateSingleflight) -> Self {
        self.populate_singleflight = Some(singleflight);

        self
    }

    /// # Panics
    ///
    /// Panics if any required field (`pool`, `registry`, `hook_runner`,
    /// `config`, `config_dir`, `email_renderer`, `login_limiter`, etc.)
    /// was not set on the builder.
    #[must_use]
    pub fn build(self) -> ContentServiceDeps {
        ContentServiceDeps {
            pool: self.pool.expect("pool is required"),
            registry: self.registry.expect("registry is required"),
            hook_runner: self.hook_runner.expect("hook_runner is required"),
            config: self.config.expect("config is required"),
            config_dir: self.config_dir.expect("config_dir is required"),
            email_renderer: self.email_renderer.expect("email_renderer is required"),
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
            cache: self.cache.expect("cache is required"),
            token_provider: self.token_provider.expect("token_provider is required"),
            password_provider: self
                .password_provider
                .expect("password_provider is required"),
            invalidation_transport: self.invalidation_transport,
            populate_singleflight: self.populate_singleflight,
        }
    }
}
