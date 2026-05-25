//! Shared helper functions used across multiple command handlers.

use std::sync::Arc;

use anyhow::{Context as _, Result};
#[cfg(unix)]
use std::io;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tracing::{info, warn};

use crate::{
    config::CrapConfig,
    core::Registry,
    db::{DbPool, migrate, pool},
    hooks::{self, HookRunner},
};

#[cfg(unix)]
use tokio::select;
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;

/// Load config, init Lua, create pool, and sync schema. Shared by user, export, import commands.
///
/// # Errors
///
/// Returns an error if config loading, Lua init, pool creation, or schema
/// sync fails.
pub fn load_config_and_sync(config_dir: &Path) -> Result<(DbPool, Arc<Registry>)> {
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
pub fn init_stack(config_dir: &Path) -> Result<(CrapConfig, Arc<Registry>, DbPool)> {
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

/// Run `on_init` hooks if configured. Failure aborts startup.
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

/// Send a signal to a process by PID.
///
/// Returns `Ok(())` if `kill(2)` returned 0, otherwise an error wrapping the
/// underlying OS error. The single canonical wrapper around `libc::kill` —
/// other call sites should go through this helper rather than calling
/// `libc::kill` directly.
#[cfg(unix)]
pub fn send_signal(pid: u32, sig: i32) -> Result<()> {
    let pid_i32 = i32::try_from(pid).context("PID too large for i32")?;
    // SAFETY: kill(2) is safe to call with any pid/signal combination.
    let ret = unsafe { libc::kill(pid_i32, sig) };

    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
            .with_context(|| format!("Failed to send signal {sig} to PID {pid}"))
    }
}

/// Check if a process with the given PID is running.
///
/// Sends signal 0 (no-op probe); succeeds iff the kernel can deliver to that PID.
#[cfg(unix)]
pub fn is_process_running(pid: u32) -> bool {
    send_signal(pid, 0).is_ok()
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
