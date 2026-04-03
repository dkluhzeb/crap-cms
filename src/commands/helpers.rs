//! Shared helper functions used across multiple command handlers.

use anyhow::{Context as _, Result};
use std::path::Path;
use tracing::warn;

use crate::{
    config::CrapConfig,
    core::SharedRegistry,
    db::{DbPool, migrate, pool},
    hooks,
};

/// Load config, init Lua, create pool, and sync schema. Shared by user, export, import commands.
pub fn load_config_and_sync(config_dir: &Path) -> Result<(DbPool, SharedRegistry)> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;

    // Check crap_version compatibility
    if let Some(warning) = cfg.check_version() {
        warn!("{}", warning);
    }

    let registry = hooks::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;
    let pool = pool::create_pool(&config_dir, &cfg).context("Failed to create database pool")?;

    migrate::sync_all(&pool, &registry, &cfg.locale).context("Failed to sync database schema")?;

    Ok((pool, registry))
}
