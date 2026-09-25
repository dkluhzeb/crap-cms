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
//!
//! # The Lua surface
//!
//! The Lua hook/CRUD surface deliberately does **not** hold an `AppInfra`:
//! `AppInfra` owns the [`HookRunner`], which owns the Lua VMs, so a VM holding
//! `Arc<AppInfra>` would be a reference cycle (and the bundle is assembled
//! *after* the runner at boot). Lua instead receives the same values through
//! the hook contract: VM-stable pieces (registry, locale config, invalidation
//! transport, populate singleflight, per-VM storage) are threaded into
//! `HookRunner::builder` from the same boot wiring and bundled per-VM as
//! `hooks::lifecycle::LuaVmInfra`; per-call pieces (event transport, cache,
//! post-commit event queues) flow from the calling surface's
//! `ServiceContext` — itself built via
//! [`ServiceContextBuilder::infra`](crate::service::ServiceContextBuilder::infra) —
//! so conn-mode writes keep their queue-until-commit semantics. There is one
//! source of truth; Lua consumes it indirectly by design.

use std::{path::Path, sync::Arc};

use anyhow::Result;

use crate::{
    config::{CrapConfig, DEFAULT_TOKEN_EXPIRY, LocaleConfig, PasswordPolicy},
    core::{
        Readiness, Registry, SharedCache, SharedEventTransport, SharedInvalidationTransport,
        SharedStorage, SharedTokenProvider,
        auth::JwtTokenProvider,
        cache::{create_cache_with_lease, warn_if_custom_cache_multi_vm},
        email::EmailRenderer,
        event::InProcessInvalidationBus,
        upload::FALLBACK_MAX_ATTEMPTS,
    },
    db::{DbPool, SharedPopulateSingleflight, Singleflight},
    hooks::HookRunner,
    service::EmailContext,
};

/// Parameters for [`AppInfra::standalone`].
#[doc(hidden)]
pub struct StandaloneInfra<'a> {
    pub pool: DbPool,
    pub registry: Arc<Registry>,
    pub hook_runner: HookRunner,
    pub storage: SharedStorage,
    /// `None` → an ephemeral placeholder provider, for surfaces that never
    /// issue or validate tokens (standalone MCP, the job worker).
    pub token_provider: Option<SharedTokenProvider>,
    /// Live mutation-event transport, built from config
    /// (`create_live_transports`) so a standalone process's writes reach
    /// `serve`'s subscribers over Redis. `None` = live events off.
    pub event_transport: Option<SharedEventTransport>,
    /// User-invalidation transport from the same config wiring. `None` → a
    /// fresh in-process bus (no cross-process delivery; fine for tests).
    pub invalidation_transport: Option<SharedInvalidationTransport>,
    pub config: &'a CrapConfig,
    pub config_dir: &'a Path,
}

impl AppInfra {
    /// Start building an [`AppInfra`]. Every field is required except
    /// `event_transport` (defaults to `None` = live updates disabled).
    #[must_use]
    pub fn builder() -> AppInfraBuilder {
        AppInfraBuilder::default()
    }

    /// Assemble a standalone bundle from core deps plus config — for processes
    /// that build their own state instead of sharing the boot bundle (the stdio
    /// MCP transport, the `work` job worker, a CLI command writing through the
    /// service layer) and for test fixtures. The populate
    /// singleflight is process-local to this bundle; the live transports come
    /// from the caller (config-built for real processes so Redis-backed writes
    /// reach `serve`'s subscribers; `None` for tests).
    ///
    /// Doc-hidden `pub` so integration tests can build fixture bundles; not
    /// part of the supported API.
    #[doc(hidden)]
    pub fn standalone(p: StandaloneInfra<'_>) -> Result<Arc<Self>> {
        let email = EmailContext {
            email_config: p.config.email.clone(),
            email_renderer: Arc::new(EmailRenderer::new(p.config_dir)?),
            server_config: p.config.server.clone(),
            email_max_attempts: p.config.jobs.system_email_max_attempts(),
        };

        let token_provider = p
            .token_provider
            .unwrap_or_else(|| Arc::new(JwtTokenProvider::new("standalone-unused-token-provider")));

        let invalidation_transport = p
            .invalidation_transport
            .unwrap_or_else(|| Arc::new(InProcessInvalidationBus::new()));

        // Lease taken before `p.hook_runner` moves into the builder below.
        warn_if_custom_cache_multi_vm(p.config);
        let cache = create_cache_with_lease(&p.config.cache, p.hook_runner.lua_lease())?;

        Ok(Arc::new(
            Self::builder()
                .pool(p.pool)
                .registry(p.registry)
                .hook_runner(p.hook_runner)
                .cache(cache)
                .storage(p.storage)
                .event_transport(p.event_transport)
                .invalidation_transport(invalidation_transport)
                .token_provider(token_provider)
                .email(email)
                .locale_config(p.config.locale.clone())
                .password_policy(p.config.auth.password_policy.clone())
                .image_max_attempts(p.config.jobs.system_image_max_attempts())
                .token_expiry(p.config.auth.token_expiry)
                .populate_singleflight(Arc::new(Singleflight::new()))
                // Ready on construction: a standalone bundle belongs to a
                // process whose startup work is finished by the time it is
                // assembled. The one that does defer work — the `work`
                // worker's scheduler — marks the same flag again after its
                // stale-job recovery, which is idempotent.
                .readiness(Readiness::ready())
                .build(),
        ))
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
    readiness: Option<Readiness>,
    image_max_attempts: Option<u32>,
    token_expiry: Option<u64>,
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

    /// Shared readiness flag, marked once startup recovery has finished and
    /// read by the readiness probe. Defaults to a fresh (not-yet-ready) flag
    /// for bundles whose process has no startup recovery to wait on.
    #[must_use]
    pub fn readiness(mut self, readiness: Readiness) -> Self {
        self.readiness = Some(readiness);
        self
    }

    /// `max_attempts` for the image-conversion jobs a write queues. Defaults to
    /// [`FALLBACK_MAX_ATTEMPTS`] for a bundle assembled without config.
    #[must_use]
    pub fn image_max_attempts(mut self, attempts: u32) -> Self {
        self.image_max_attempts = Some(attempts);
        self
    }

    /// The default session token lifetime (`[auth] token_expiry`), for auth
    /// collections that set no `token_expiry` of their own. Defaults to
    /// [`DEFAULT_TOKEN_EXPIRY`] for a bundle assembled without config.
    #[must_use]
    pub fn token_expiry(mut self, seconds: u64) -> Self {
        self.token_expiry = Some(seconds);
        self
    }

    /// # Panics
    ///
    /// Panics if any required field (everything except `event_transport`,
    /// `readiness`, `image_max_attempts` and `token_expiry`) was not set on
    /// the builder.
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
            readiness: self.readiness.unwrap_or_default(),
            image_max_attempts: self.image_max_attempts.unwrap_or(FALLBACK_MAX_ATTEMPTS),
            token_expiry: self.token_expiry.unwrap_or(DEFAULT_TOKEN_EXPIRY),
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
    /// Set once startup recovery has finished. The readiness probe answers
    /// 503 until then, so an orchestrator doesn't route traffic to — or
    /// advance a rolling deploy past — a node that is still reclaiming the
    /// job rows a previous process left `running`.
    pub readiness: Readiness,
    /// `max_attempts` the image-conversion jobs a write queues are inserted
    /// with (from `[jobs] system_image_max_attempts`). Threaded onto every
    /// write context so a write that queues a conversion without a file of its
    /// own — publishing a draft's file — uses the configured retry budget
    /// instead of the fallback.
    pub image_max_attempts: u32,
    /// The default session token lifetime in seconds (`[auth] token_expiry`).
    /// An auth collection's own `token_expiry` overrides it; minting resolves
    /// the two in one place (`service::auth::mint_session`).
    pub token_expiry: u64,
}
