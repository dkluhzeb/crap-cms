//! Shared fixtures for `#[cfg(test)] mod tests` blocks across `mcp::tools`.
//!
//! Co-located tests in each tool file pull from here for `Registry`
//! construction and `ToolExecCtx` building so we don't duplicate the
//! boilerplate per file.

use std::sync::Arc;

use crate::{
    config::CrapConfig,
    core::{CollectionDefinition, Registry, collection::GlobalDefinition},
    db::DbPool,
    hooks::lifecycle::HookRunner,
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

/// Build a minimal `ToolExecCtx` for tests — no event/invalidation/cache
/// wiring, since tests focus on dispatch and per-tool happy paths.
pub(in crate::mcp::tools) fn make_exec_ctx<'a>(
    pool: &'a DbPool,
    registry: &'a Arc<Registry>,
    runner: &'a HookRunner,
    config: &'a CrapConfig,
) -> ToolExecCtx<'a> {
    ToolExecCtx {
        registry,
        pool,
        runner,
        config,
        event_transport: None,
        invalidation_transport: None,
        cache: None,
        client_label: "(test)",
    }
}
