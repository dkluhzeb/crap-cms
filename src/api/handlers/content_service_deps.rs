//! [`ContentServiceDeps`] — the dependency bundle handed to
//! [`ContentService::new`](super::ContentService::new) — and its builder.

use std::{path::PathBuf, sync::Arc};

use crate::{
    config::CrapConfig,
    core::{
        Registry, SharedCache, SharedEventTransport, SharedInvalidationTransport,
        SharedPasswordProvider, SharedStorage, SharedTokenProvider, email::EmailRenderer,
        event::InProcessInvalidationBus, rate_limit::LoginRateLimiter,
    },
    db::{DbPool, Singleflight, query::SharedPopulateSingleflight},
    hooks::HookRunner,
    service::{AppInfra, EmailContext},
};

/// Dependencies for constructing a `ContentService`.
///
/// All process-stable infrastructure lives in [`AppInfra`]; only the genuinely
/// per-surface bits (config, rate limiters, password provider) sit alongside it.
/// The builder still accepts the individual infra dependencies as setters —
/// [`ContentServiceDepsBuilder::build`] assembles them into an [`AppInfra`] when
/// no pre-built one was supplied via [`ContentServiceDepsBuilder::infra`] — so
/// test construction stays ergonomic while production threads in the shared
/// boot-time bundle.
pub struct ContentServiceDeps {
    pub infra: Arc<AppInfra>,
    pub config: CrapConfig,
    pub config_dir: PathBuf,
    pub login_limiter: Arc<LoginRateLimiter>,
    pub ip_login_limiter: Arc<LoginRateLimiter>,
    pub mfa_limiter: Arc<LoginRateLimiter>,
    pub ip_mfa_limiter: Arc<LoginRateLimiter>,
    pub forgot_password_limiter: Arc<LoginRateLimiter>,
    pub ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    pub password_provider: SharedPasswordProvider,
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
    mfa_limiter: Option<Arc<LoginRateLimiter>>,
    ip_mfa_limiter: Option<Arc<LoginRateLimiter>>,
    forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    ip_forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    storage: Option<SharedStorage>,
    cache: Option<SharedCache>,
    token_provider: Option<SharedTokenProvider>,
    password_provider: Option<SharedPasswordProvider>,
    invalidation_transport: Option<SharedInvalidationTransport>,
    populate_singleflight: Option<SharedPopulateSingleflight>,
    infra: Option<Arc<AppInfra>>,
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
            mfa_limiter: None,
            ip_mfa_limiter: None,
            forgot_password_limiter: None,
            ip_forgot_password_limiter: None,
            storage: None,
            cache: None,
            token_provider: None,
            password_provider: None,
            invalidation_transport: None,
            populate_singleflight: None,
            infra: None,
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
    pub fn mfa_limiter(mut self, mfa_limiter: Arc<LoginRateLimiter>) -> Self {
        self.mfa_limiter = Some(mfa_limiter);

        self
    }

    #[must_use]
    pub fn ip_mfa_limiter(mut self, ip_mfa_limiter: Arc<LoginRateLimiter>) -> Self {
        self.ip_mfa_limiter = Some(ip_mfa_limiter);

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

    /// Supply a pre-assembled process-stable [`AppInfra`] (the production boot
    /// path). When set, the service uses it instead of assembling one from the
    /// individual dependency fields.
    #[must_use]
    pub fn infra(mut self, infra: Arc<AppInfra>) -> Self {
        self.infra = Some(infra);

        self
    }

    /// # Panics
    ///
    /// Panics if a required field is missing. When no pre-built [`AppInfra`] was
    /// supplied via [`Self::infra`], the individual infra setters (`pool`,
    /// `registry`, `hook_runner`, `cache`, `storage`, `token_provider`,
    /// `email_renderer`) are all required so one can be assembled. The
    /// per-surface fields (`config`, `config_dir`, the rate limiters,
    /// `password_provider`) are always required.
    #[must_use]
    pub fn build(self) -> ContentServiceDeps {
        let config = self.config.expect("config is required");

        // Use the pre-built infra (production boot path) or assemble one from the
        // individual dependency setters (test construction).
        let infra = self.infra.unwrap_or_else(|| {
            let invalidation_transport = self
                .invalidation_transport
                .unwrap_or_else(|| Arc::new(InProcessInvalidationBus::new()));
            let populate_singleflight = self
                .populate_singleflight
                .unwrap_or_else(|| Arc::new(Singleflight::new()));

            Arc::new(
                AppInfra::builder()
                    .pool(self.pool.expect("pool is required"))
                    .registry(self.registry.expect("registry is required"))
                    .hook_runner(self.hook_runner.expect("hook_runner is required"))
                    .cache(self.cache.expect("cache is required"))
                    .storage(self.storage.expect("storage is required"))
                    .event_transport(self.event_transport)
                    .invalidation_transport(invalidation_transport)
                    .token_provider(self.token_provider.expect("token_provider is required"))
                    .email(EmailContext {
                        email_config: config.email.clone(),
                        email_renderer: self.email_renderer.expect("email_renderer is required"),
                        server_config: config.server.clone(),
                        email_max_attempts: config.jobs.system_email_max_attempts(),
                    })
                    .locale_config(config.locale.clone())
                    .password_policy(config.auth.password_policy.clone())
                    .populate_singleflight(populate_singleflight)
                    .build(),
            )
        });

        // Defaulted from config when unset (test constructions): the MFA
        // guess budget mirrors the login thresholds, in its own keyspace.
        let mfa_limiter = self.mfa_limiter.unwrap_or_else(|| {
            Arc::new(LoginRateLimiter::new(
                config.auth.max_login_attempts,
                config.auth.login_lockout_seconds,
            ))
        });
        let ip_mfa_limiter = self.ip_mfa_limiter.unwrap_or_else(|| {
            Arc::new(LoginRateLimiter::new(
                config.auth.max_ip_login_attempts,
                config.auth.login_lockout_seconds,
            ))
        });

        ContentServiceDeps {
            infra,
            config,
            config_dir: self.config_dir.expect("config_dir is required"),
            login_limiter: self.login_limiter.expect("login_limiter is required"),
            ip_login_limiter: self.ip_login_limiter.expect("ip_login_limiter is required"),
            mfa_limiter,
            ip_mfa_limiter,
            forgot_password_limiter: self
                .forgot_password_limiter
                .expect("forgot_password_limiter is required"),
            ip_forgot_password_limiter: self
                .ip_forgot_password_limiter
                .expect("ip_forgot_password_limiter is required"),
            password_provider: self
                .password_provider
                .expect("password_provider is required"),
        }
    }
}
