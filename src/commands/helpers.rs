//! Shared helper functions used across multiple command handlers.

use anyhow::{Context as _, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tracing::{info, warn};

use crate::{
    config::CrapConfig,
    core::SharedRegistry,
    db::{DbPool, migrate, pool},
    hooks,
    hooks::HookRunner,
};

#[cfg(unix)]
use tokio::select;
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;

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

/// Load config, init Lua, create pool, sync schema, and return all three.
/// Used by commands that need the config (jobs, trash).
pub fn init_stack(config_dir: &Path) -> Result<(CrapConfig, SharedRegistry, DbPool)> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());
    let cfg = CrapConfig::load(&config_dir)?;
    let registry = hooks::init_lua(&config_dir, &cfg)?;
    let pool = pool::create_pool(&config_dir, &cfg)?;

    migrate::sync_all(&pool, &registry, &cfg.locale)?;

    Ok((cfg, registry, pool))
}

/// Load, validate config, check version, and prune old log files.
/// Shared by serve and work commands.
pub fn load_and_validate_config(config_dir: &Path) -> Result<CrapConfig> {
    let cfg = CrapConfig::load(config_dir)?;
    cfg.validate()?;

    if let Some(warning) = cfg.check_version() {
        warn!("{}", warning);
    }

    if cfg.logging.file {
        let log_dir = cfg.log_dir(config_dir);

        if log_dir.exists() {
            match super::logs::prune_old_logs(&log_dir, cfg.logging.max_files) {
                Ok(0) => {}
                Ok(n) => info!("Pruned {n} old log file(s)"),
                Err(e) => warn!("Failed to prune old log files: {e}"),
            }
        }
    }

    Ok(cfg)
}

/// Run on_init hooks if configured. Failure aborts startup.
pub fn run_on_init_hooks(cfg: &CrapConfig, pool: &DbPool, hook_runner: &HookRunner) -> Result<()> {
    if cfg.hooks.on_init.is_empty() {
        return Ok(());
    }

    info!("Running on_init hooks...");

    let mut conn = pool.get().context("DB connection for on_init")?;
    let tx = conn.transaction().context("Transaction for on_init")?;

    hook_runner
        .run_system_hooks_with_conn(&cfg.hooks.on_init, &tx)
        .context("on_init hooks failed")?;

    tx.commit().context("Commit on_init transaction")?;

    info!("on_init hooks completed");

    Ok(())
}

/// Spawn a task that listens for shutdown signals (SIGINT/SIGTERM) and cancels the token.
/// `label` is used in log messages (e.g. "worker" or empty for server).
pub fn spawn_shutdown_signal(shutdown: CancellationToken, label: &'static str) {
    let prefix = if label.is_empty() {
        String::new()
    } else {
        format!(" {label}")
    };

    tokio::spawn(async move {
        #[cfg(unix)]
        {
            let mut sigterm =
                signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");

            select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Received SIGINT, shutting down{prefix} gracefully...");
                }
                _ = sigterm.recv() => {
                    info!("Received SIGTERM, shutting down{prefix} gracefully...");
                }
            }
        }

        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            info!("Received shutdown signal, shutting down{prefix} gracefully...");
        }

        shutdown.cancel();

        #[cfg(unix)]
        {
            let mut sigterm =
                signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");

            select! {
                _ = tokio::signal::ctrl_c() => {
                    warn!("Received second SIGINT, forcing exit");
                }
                _ = sigterm.recv() => {
                    warn!("Received second SIGTERM, forcing exit");
                }
            }
        }

        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            warn!("Received second shutdown signal, forcing exit");
        }

        std::process::exit(1);
    });
}

// ── PID file helpers ─────────────────────────────────────────────────────

/// Path to a named PID file within the config directory's data dir.
pub fn pid_file_path(config_dir: &Path, filename: &str) -> PathBuf {
    config_dir.join("data").join(filename)
}

/// Write a PID to the named PID file.
pub fn write_pid_file(config_dir: &Path, filename: &str, pid: u32) -> Result<()> {
    let path = pid_file_path(config_dir, filename);
    let _ = fs::create_dir_all(path.parent().expect("pid path has parent"));

    fs::write(&path, pid.to_string())
        .with_context(|| format!("Failed to write PID file: {}", path.display()))?;

    Ok(())
}

/// Remove the named PID file on clean shutdown.
pub fn remove_pid_file(config_dir: &Path, filename: &str) {
    let path = pid_file_path(config_dir, filename);

    if path.exists() {
        let _ = fs::remove_file(&path);
    }
}

/// Read the PID from the named PID file.
#[cfg(unix)]
pub fn read_pid(config_dir: &Path, filename: &str) -> Option<u32> {
    let path = pid_file_path(config_dir, filename);

    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Check if a process with the given PID is running.
#[cfg(unix)]
pub fn is_process_running(pid: u32) -> bool {
    let Ok(pid_i32) = i32::try_from(pid) else {
        return false;
    };

    unsafe { libc::kill(pid_i32, 0) == 0 }
}

/// Check if a PID file exists and warn if the process is still running.
#[cfg(unix)]
pub fn check_existing_pid(config_dir: &Path, filename: &str) {
    let path = pid_file_path(config_dir, filename);

    if let Ok(contents) = fs::read_to_string(&path)
        && let Ok(pid) = contents.trim().parse::<u32>()
        && is_process_running(pid)
    {
        warn!(
            "PID file exists with PID {} — another instance may be running",
            pid
        );
    }
}
