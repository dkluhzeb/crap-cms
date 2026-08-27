//! `mcp` command — start the MCP stdio server.

use std::{
    path::Path,
    sync::{Arc, OnceLock},
};

use anyhow::{Context as _, Result};
use tracing::info;

use crate::{
    commands::helpers::create_live_transports,
    config::CrapConfig,
    core::upload::create_storage_with_lease,
    db::{migrate, pool},
    hooks::{self, HookRunner},
    mcp,
    service::{AppInfra, StandaloneInfra},
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

    // Live transports from config: with Redis live updates, stdio-MCP writes
    // must publish events and user-invalidation that reach the `serve`
    // process's subscribers (in-process transports are no-ops here).
    let (event_transport, invalidation_transport) = create_live_transports(&cfg)?;

    let hook_runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&cfg)
        .invalidation_transport(invalidation_transport.clone())
        .build()?;

    let storage = create_storage_with_lease(&config_dir, &cfg.upload, hook_runner.lua_lease())
        .context("Failed to create storage backend")?;

    info!("MCP server starting (stdio mode)");

    // Standalone stdio builds its own process-stable bundle, carrying the
    // config-built live transports. MCP uses only the core subset; upload
    // files are still cleaned on hard-delete via `storage`.
    let infra = AppInfra::standalone(StandaloneInfra {
        pool,
        registry,
        hook_runner,
        storage,
        token_provider: None,
        event_transport,
        invalidation_transport: Some(invalidation_transport),
        config: &cfg,
        config_dir: &config_dir,
    })?;

    let server = mcp::McpServer {
        infra,
        config: cfg,
        config_dir,
        client_name: OnceLock::new(),
        transport_label: "(stdio)",
    };

    mcp::run_stdio(server).await;

    Ok(())
}
