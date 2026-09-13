//! Test-only construction of a fully wired [`McpServer`].
//!
//! Backed by a real `SQLite` pool, hook runner, and local-disk storage so
//! the tests exercise production wiring rather than a stub.

use std::sync::{Arc, OnceLock};

use tempfile::TempDir;

use crate::{
    config::CrapConfig,
    core::{
        Registry, SharedStorage, collection::CollectionDefinition, upload::storage::LocalStorage,
    },
    db::{migrate, pool},
    hooks::HookRunner,
    mcp::McpServer,
    service::{AppInfra, StandaloneInfra},
};

/// Build a server exposing exactly `collections`.
pub(in crate::mcp) fn make_server_with(
    collections: &[CollectionDefinition],
) -> (TempDir, McpServer) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut config = CrapConfig::test_default();
    config.database.path = "test.db".to_string();

    let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

    let shared = Registry::shared();
    {
        let mut reg = shared.write().unwrap();
        for def in collections {
            reg.register_collection(def.clone());
        }
    }

    migrate::sync_all(&db_pool, &shared.read().unwrap(), &config.locale).expect("sync schema");

    let registry = Registry::snapshot(&shared);
    let runner = HookRunner::builder()
        .config_dir(tmp.path())
        .registry(Arc::clone(&registry))
        .config(&config)
        .build()
        .expect("hook runner");

    // Real local-disk storage rooted at `<tmp>/uploads` so hard-delete file
    // cleanup is exercised end-to-end (mirrors production wiring).
    let storage: SharedStorage = Arc::new(LocalStorage::new(tmp.path().join("uploads")));
    let infra = AppInfra::standalone(StandaloneInfra {
        pool: db_pool,
        registry,
        hook_runner: runner,
        storage,
        token_provider: None,
        event_transport: None,
        invalidation_transport: None,
        config: &config,
        config_dir: tmp.path(),
    })
    .expect("build test infra");

    let server = McpServer {
        infra,
        config,
        config_dir: tmp.path().to_path_buf(),
        client_name: OnceLock::new(),
        transport_label: "(test)",
    };

    (tmp, server)
}

/// Build a server with a single `posts` collection.
pub(in crate::mcp) fn make_server() -> (TempDir, McpServer) {
    make_server_with(&[CollectionDefinition::new("posts")])
}
