//! `mcp` command — start the MCP stdio server.

use std::{
    path::Path,
    sync::{Arc, OnceLock},
};

use anyhow::{Context as _, Result};
use tracing::info;

use crate::{
    config::CrapConfig,
    core::upload::create_storage_with_lease,
    db::{migrate, pool},
    hooks::{self, HookRunner},
    mcp,
};

/// Start the MCP server in stdio mode.
///
/// # Errors
///
/// Returns an error if config loading, Lua init, pool creation, schema sync,
/// or the MCP server hits an unrecoverable runtime error.
#[cfg(not(tarpaulin_include))] // async server startup, requires interactive stdio
pub async fn run(config_dir: &Path) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    // Use stderr for logging since stdout is the MCP transport
    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;

    if let Some(warning) = cfg.check_version() {
        eprintln!("Warning: {warning}");
    }

    let registry = hooks::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;

    let pool = pool::create_pool(&config_dir, &cfg).context("Failed to create database pool")?;

    migrate::sync_all(&pool, &registry, &cfg.locale).context("Failed to sync database schema")?;

    let hook_runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&cfg)
        .build()?;

    let storage = create_storage_with_lease(&config_dir, &cfg.upload, hook_runner.lua_lease())
        .context("Failed to create storage backend")?;

    info!("MCP server starting (stdio mode)");

    let server = mcp::McpServer {
        pool,
        registry,
        runner: hook_runner,
        config: cfg,
        config_dir,
        // Stdio MCP runs standalone — no live-update streams or cache.
        event_transport: None,
        invalidation_transport: None,
        cache: None,
        // Upload files are still cleaned on hard-delete via the storage backend.
        storage: Some(storage),
        client_name: OnceLock::new(),
        transport_label: "(stdio)",
    };

    mcp::run_stdio(server).await;

    Ok(())
}
