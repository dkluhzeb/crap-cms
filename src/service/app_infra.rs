//! Process-stable application infrastructure, assembled once at boot.
//!
//! `AppInfra` bundles every dependency that is constructed at startup and never
//! varies per request — the connection pool, hook runner, caches, transports,
//! providers, and the config-derived infra (email context, locale config,
//! password policy). Each surface (gRPC, MCP, admin, scheduler) holds an
//! `Arc<AppInfra>` and threads it into a [`ServiceContext`] via
//! [`ServiceContextBuilder::infra`](crate::service::ServiceContextBuilder::infra)
//! in one call, so no operation can build a context that silently forgets an
//! infrastructure field (the bug class the chokepoint audits kept finding).
//!
//! Per-*call* state — the authenticated user, the target slug/definition, the
//! active transaction, access-override flags — is **not** here; it lives on the
//! `ServiceContext` per operation.

use std::sync::Arc;

use crate::{
    config::{LocaleConfig, PasswordPolicy},
    core::{
        Registry, SharedCache, SharedEventTransport, SharedInvalidationTransport, SharedStorage,
        SharedTokenProvider,
    },
    db::{DbPool, SharedPopulateSingleflight},
    hooks::HookRunner,
    service::EmailContext,
};

impl AppInfra {
    /// Start building an [`AppInfra`]. Every field is required except
    /// `event_transport` (defaults to `None` = live updates disabled).
    #[must_use]
    pub fn builder() -> AppInfraBuilder {
        AppInfraBuilder::default()
    }
}

/// Builder for [`AppInfra`]. Assembled once at boot
/// ([`bootstrap_startup`](crate::commands)) and, as a fallback, inside
/// `ContentServiceDeps` when no pre-built infra was supplied (test construction).
#[derive(Default)]
pub struct AppInfraBuilder {
    pool: Option<DbPool>,
    registry: Option<Arc<Registry>>,
    hook_runner: Option<HookRunner>,
    cache: Option<SharedCache>,
    storage: Option<SharedStorage>,
    event_transport: Option<SharedEventTransport>,
    invalidation_transport: Option<SharedInvalidationTransport>,
    token_provider: Option<SharedTokenProvider>,
    email: Option<EmailContext>,
    locale_config: Option<LocaleConfig>,
    password_policy: Option<PasswordPolicy>,
    populate_singleflight: Option<SharedPopulateSingleflight>,
}

impl AppInfraBuilder {
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
    pub fn cache(mut self, cache: SharedCache) -> Self {
        self.cache = Some(cache);
        self
    }

    #[must_use]
    pub fn storage(mut self, storage: SharedStorage) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Live mutation-event transport. `None` (the default) = live updates off.
    #[must_use]
    pub fn event_transport(mut self, transport: Option<SharedEventTransport>) -> Self {
        self.event_transport = transport;
        self
    }

    #[must_use]
    pub fn invalidation_transport(mut self, transport: SharedInvalidationTransport) -> Self {
        self.invalidation_transport = Some(transport);
        self
    }

    #[must_use]
    pub fn token_provider(mut self, token_provider: SharedTokenProvider) -> Self {
        self.token_provider = Some(token_provider);
        self
    }

    #[must_use]
    pub fn email(mut self, email: EmailContext) -> Self {
        self.email = Some(email);
        self
    }

    #[must_use]
    pub fn locale_config(mut self, locale_config: LocaleConfig) -> Self {
        self.locale_config = Some(locale_config);
        self
    }

    #[must_use]
    pub fn password_policy(mut self, password_policy: PasswordPolicy) -> Self {
        self.password_policy = Some(password_policy);
        self
    }

    #[must_use]
    pub fn populate_singleflight(mut self, singleflight: SharedPopulateSingleflight) -> Self {
        self.populate_singleflight = Some(singleflight);
        self
    }

    /// # Panics
    ///
    /// Panics if any required field (everything except `event_transport`) was
    /// not set on the builder.
    #[must_use]
    pub fn build(self) -> AppInfra {
        AppInfra {
            pool: self.pool.expect("pool is required"),
            registry: self.registry.expect("registry is required"),
            hook_runner: self.hook_runner.expect("hook_runner is required"),
            cache: self.cache.expect("cache is required"),
            storage: self.storage.expect("storage is required"),
            event_transport: self.event_transport,
            invalidation_transport: self
                .invalidation_transport
                .expect("invalidation_transport is required"),
            token_provider: self.token_provider.expect("token_provider is required"),
            email: self.email.expect("email is required"),
            locale_config: self.locale_config.expect("locale_config is required"),
            password_policy: self.password_policy.expect("password_policy is required"),
            populate_singleflight: self
                .populate_singleflight
                .expect("populate_singleflight is required"),
        }
    }
}

/// Immutable bundle of process-stable dependencies, built once at startup and
/// shared (as `Arc<AppInfra>`) by every surface. See the module docs.
pub struct AppInfra {
    /// Backend-agnostic connection pool (read/write split lives inside it).
    pub pool: DbPool,
    /// Resolved collection/global/job registry snapshot.
    pub registry: Arc<Registry>,
    /// Runtime hook runner (elastic Lua VM pool).
    pub hook_runner: HookRunner,
    /// Cross-request populate cache (`NoneCache` when caching is disabled).
    pub cache: SharedCache,
    /// Upload storage backend.
    pub storage: SharedStorage,
    /// Live mutation-event transport (`None` when live updates are disabled).
    pub event_transport: Option<SharedEventTransport>,
    /// User-invalidation transport (always present; publishing is a no-op when
    /// live updates are off).
    pub invalidation_transport: SharedInvalidationTransport,
    /// JWT token provider.
    pub token_provider: SharedTokenProvider,
    /// Verification/reset email context (config + renderer + server config).
    pub email: EmailContext,
    /// Locale configuration (from `[locale]`).
    pub locale_config: LocaleConfig,
    /// Password policy for auth-collection writes (from `[auth.password_policy]`).
    pub password_policy: PasswordPolicy,
    /// Process-wide singleflight for deduplicating concurrent populate
    /// cache-miss fetches across requests.
    pub populate_singleflight: SharedPopulateSingleflight,
}
