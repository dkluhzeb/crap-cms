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
