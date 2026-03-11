//! `mcp` command — start the MCP stdio server.

use anyhow::{Context as _, Result};
use std::path::Path;
use tracing::info;

/// Start the MCP server in stdio mode.
#[cfg(not(tarpaulin_include))] // async server startup, requires interactive stdio
pub async fn run(config_dir: &Path) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    // Use stderr for logging since stdout is the MCP transport
    let cfg = crate::config::CrapConfig::load(&config_dir).context("Failed to load config")?;

    if let Some(warning) = cfg.check_version() {
        eprintln!("Warning: {}", warning);
    }

    let registry =
        crate::hooks::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;

    let pool = crate::db::pool::create_pool(&config_dir, &cfg)
        .context("Failed to create database pool")?;

    crate::db::migrate::sync_all(&pool, &registry, &cfg.locale)
        .context("Failed to sync database schema")?;

    let hook_runner = crate::hooks::lifecycle::HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry.clone())
        .config(&cfg)
        .build()?;

    let registry_snapshot = crate::core::Registry::snapshot(&registry);

    info!("MCP server starting (stdio mode)");

    let server = crate::mcp::McpServer {
        pool,
        registry: registry_snapshot,
        runner: hook_runner,
        config: cfg,
        config_dir,
    };

    crate::mcp::stdio::run_stdio(server).await;

    Ok(())
}
