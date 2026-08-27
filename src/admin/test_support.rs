//! Doc-hidden helper for admin tests. Exposed (`#[doc(hidden)] pub`) only so the
//! integration tests in `tests/admin_*.rs` — which build an [`AdminState`]
//! directly — can assemble its `infra` bundle without duplicating the full
//! `AppInfra` builder at every site.

use std::{path::Path, sync::Arc};

use crate::{
    config::CrapConfig,
    core::{
        Registry, SharedStorage, SharedTokenProvider, cache::NoneCache, email::EmailRenderer,
        event::InProcessInvalidationBus,
    },
    db::{DbPool, Singleflight},
    hooks::HookRunner,
    service::{AppInfra, EmailContext},
};

/// Assemble an [`AppInfra`] from admin test dependencies. Live updates are off
/// and the cache is a `NoneCache`; `token_provider` is passed in so it matches
/// the test's `jwt_secret`.
#[doc(hidden)]
#[must_use]
pub fn test_infra(
    pool: DbPool,
    registry: Arc<Registry>,
    hook_runner: HookRunner,
    storage: SharedStorage,
    token_provider: SharedTokenProvider,
    config: &CrapConfig,
    config_dir: &Path,
) -> Arc<AppInfra> {
    Arc::new(
        AppInfra::builder()
            .pool(pool)
            .registry(registry)
            .hook_runner(hook_runner)
            .cache(Arc::new(NoneCache))
            .storage(storage)
            .event_transport(None)
            .invalidation_transport(Arc::new(InProcessInvalidationBus::new()))
            .token_provider(token_provider)
            .email(EmailContext {
                email_config: config.email.clone(),
                email_renderer: Arc::new(EmailRenderer::new(config_dir).expect("email renderer")),
                server_config: config.server.clone(),
                email_max_attempts: config.jobs.system_email_max_attempts(),
            })
            .locale_config(config.locale.clone())
            .password_policy(config.auth.password_policy.clone())
            .populate_singleflight(Arc::new(Singleflight::new()))
            .build(),
    )
}
