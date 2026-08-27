//! Doc-hidden helper for admin tests. Exposed (`#[doc(hidden)] pub`) only so the
//! integration tests in `tests/admin_*.rs` — which build an [`AdminState`]
//! directly — can assemble its `infra` bundle without duplicating the
//! [`AppInfra`] construction at every site.
//!
//! [`AdminState`]: crate::admin::AdminState

use std::{path::Path, sync::Arc};

use crate::{
    config::CrapConfig,
    core::{Registry, SharedStorage, SharedTokenProvider},
    db::DbPool,
    hooks::HookRunner,
    service::{AppInfra, StandaloneInfra},
};

/// Assemble an [`AppInfra`] from admin test dependencies — a thin wrapper over
/// [`AppInfra::standalone`]. `token_provider` is passed in so it matches the
/// test's `jwt_secret`.
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
    AppInfra::standalone(StandaloneInfra {
        pool,
        registry,
        hook_runner,
        storage,
        token_provider: Some(token_provider),
        event_transport: None,
        invalidation_transport: None,
        config,
        config_dir,
    })
    .expect("build test infra")
}
