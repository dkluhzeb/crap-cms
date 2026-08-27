//! Shared fixtures for `#[cfg(test)] mod tests` blocks across `mcp::tools`.
//!
//! Co-located tests in each tool file pull from here for `Registry`
//! construction and `ToolExecCtx` building so we don't duplicate the
//! boilerplate per file.

use std::{path::Path, sync::Arc};

use crate::{
    config::CrapConfig,
    core::{
        CollectionDefinition, Registry, SharedStorage, collection::GlobalDefinition,
        upload::storage::LocalStorage,
    },
    db::DbPool,
    hooks::lifecycle::HookRunner,
    mcp::infra::standalone_infra,
};

use super::ToolExecCtx;

/// Three-entry registry: two collections (`posts`, `users`) and one
/// global (`settings`). Most tests in `mcp::tools` rely on this exact
/// shape.
pub(in crate::mcp::tools) fn make_registry() -> Registry {
    let mut reg = Registry::new();
    reg.register_collection(CollectionDefinition::new("posts"));
    reg.register_collection(CollectionDefinition::new("users"));
    reg.register_global(GlobalDefinition::new("settings"));
    reg
}

/// Build a `ToolExecCtx` for tests. Assembles a standalone `AppInfra` (MCP uses
/// only its core subset) with local-disk storage rooted at `config_dir`.
pub(in crate::mcp::tools) fn make_exec_ctx<'a>(
    pool: &DbPool,
    registry: &Arc<Registry>,
    runner: &HookRunner,
    config: &'a CrapConfig,
    config_dir: &Path,
) -> ToolExecCtx<'a> {
    let storage: SharedStorage = Arc::new(LocalStorage::new(config_dir.join("uploads")));
    let infra = standalone_infra(
        pool.clone(),
        Arc::clone(registry),
        runner.clone(),
        storage,
        config,
        config_dir,
    )
    .expect("build test infra");

    ToolExecCtx {
        infra,
        config,
        client_label: "(test)",
    }
}
