//! Doc-hidden helper for admin tests. Exposed (`#[doc(hidden)] pub`) only so the
//! integration tests in `tests/admin_*.rs` — which build an [`AdminState`]
//! directly — can assemble its `infra` bundle without duplicating the
//! [`AppInfra`] construction at every site.
//!
//! Unit tests additionally get `test_infra_with_events`, a self-contained infra
//! with an in-process event bus for asserting on published events.
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

#[cfg(test)]
pub use events::test_infra_with_events;

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

/// Crate-internal fixture for unit tests that assert on published events.
#[cfg(test)]
mod events {
    use std::sync::Arc;

    use tempfile::TempDir;

    use crate::{
        config::CrapConfig,
        core::{
            CollectionDefinition, EventReceiver, Registry, SharedEventTransport,
            event::InProcessEventBus, upload::create_storage,
        },
        db::{DbPool, migrate, pool},
        hooks::HookRunner,
        service::{AppInfra, StandaloneInfra},
    };

    /// A file-backed pool in `tmp` with `def` registered and migrated.
    fn migrated_pool(
        tmp: &TempDir,
        config: &CrapConfig,
        def: CollectionDefinition,
    ) -> (DbPool, Arc<Registry>) {
        let db_pool = pool::create_pool(tmp.path(), config).expect("create pool");

        let shared = Registry::shared();
        shared.write().unwrap().register_collection(def);
        let registry = Registry::snapshot(&shared);
        migrate::sync_all(&db_pool, &registry, &config.locale).expect("sync schema");

        (db_pool, registry)
    }

    /// A full [`AppInfra`] over `def` (temp-dir `SQLite` database, local
    /// storage, memory cache) with an in-process event bus, and a receiver
    /// subscribed to that bus before any write. The [`TempDir`] must outlive
    /// the infra.
    ///
    /// # Panics
    ///
    /// When the temp dir, database, schema, hook runner or storage can't be
    /// set up.
    #[must_use]
    pub fn test_infra_with_events(
        def: CollectionDefinition,
    ) -> (TempDir, Arc<AppInfra>, EventReceiver) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();

        let (db_pool, registry) = migrated_pool(&tmp, &config, def);

        let hook_runner = HookRunner::builder()
            .config_dir(tmp.path())
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .expect("hook runner");
        let storage = create_storage(tmp.path(), &config.upload).expect("storage");

        let events: SharedEventTransport = Arc::new(InProcessEventBus::new(16));
        let rx = events.subscribe();

        let infra = AppInfra::standalone(StandaloneInfra {
            pool: db_pool,
            registry,
            hook_runner,
            storage,
            token_provider: None,
            event_transport: Some(events),
            invalidation_transport: None,
            config: &config,
            config_dir: tmp.path(),
        })
        .expect("build test infra");

        (tmp, infra, rx)
    }
}
