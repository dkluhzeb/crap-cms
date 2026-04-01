//! `work` command — standalone job worker that processes queues without HTTP/gRPC servers.
//!
//! Used in multi-server deployments where app servers run `serve --no-scheduler`
//! and one or more dedicated workers run `work`.

use std::path::Path;
use std::process;

use anyhow::{Context as _, Result};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::cli;

use crate::{
    config::CrapConfig,
    core::{email::create_email_provider, upload::create_storage},
    db::{migrate, pool},
    hooks::{self, HookRunner},
    scheduler::{self, SchedulerParamsBuilder},
};

use std::fs;
use std::path::PathBuf;

/// PID file for the worker process (separate from server's crap.pid).
fn pid_file_path(config_dir: &Path) -> PathBuf {
    config_dir.join("data").join("crap-worker.pid")
}

fn write_pid_file(config_dir: &Path, pid: u32) -> Result<()> {
    let path = pid_file_path(config_dir);
    let _ = fs::create_dir_all(path.parent().expect("pid path has parent"));
    fs::write(&path, pid.to_string())
        .with_context(|| format!("Failed to write worker PID file: {}", path.display()))?;
    Ok(())
}

fn remove_pid_file(config_dir: &Path) {
    let path = pid_file_path(config_dir);
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
}

#[cfg(unix)]
fn read_pid(config_dir: &Path) -> Option<u32> {
    let path = pid_file_path(config_dir);
    fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

#[cfg(unix)]
fn is_process_running(pid: u32) -> bool {
    let Ok(pid_i32) = i32::try_from(pid) else {
        return false;
    };
    unsafe { libc::kill(pid_i32, 0) == 0 }
}

/// Stop a running detached worker.
#[cfg(unix)]
pub fn stop(config_dir: &Path) -> Result<()> {
    let pid = read_pid(config_dir).context(
        "No worker PID file found — is there a detached worker running?\n\
         Start one with: crap-cms work --detach",
    )?;

    if !is_process_running(pid) {
        remove_pid_file(config_dir);
        anyhow::bail!(
            "Worker process {} is not running (stale PID file removed)",
            pid
        );
    }

    unsafe { libc::kill(i32::try_from(pid).unwrap(), libc::SIGTERM) };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if !is_process_running(pid) {
            remove_pid_file(config_dir);
            cli::success(&format!("Stopped worker (PID {pid})"));
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    cli::warning(&format!(
        "Worker {pid} did not stop within 10s, sending SIGKILL"
    ));
    unsafe { libc::kill(i32::try_from(pid).unwrap(), libc::SIGKILL) };
    std::thread::sleep(std::time::Duration::from_millis(500));
    remove_pid_file(config_dir);
    cli::success(&format!("Force-stopped worker (PID {pid})"));
    Ok(())
}

/// Restart a running detached worker.
#[cfg(unix)]
pub fn restart(
    config_dir: &Path,
    queues: Option<Vec<String>>,
    concurrency: Option<usize>,
    no_cron: bool,
) -> Result<()> {
    if let Some(pid) = read_pid(config_dir) {
        if is_process_running(pid) {
            if let Err(e) = stop(config_dir) {
                tracing::debug!("stop() during restart: {e}");
            }
        } else {
            remove_pid_file(config_dir);
        }
    }
    detach(config_dir, queues, concurrency, no_cron)
}

/// Show status of a detached worker.
#[cfg(unix)]
pub fn status(config_dir: &Path) -> Result<()> {
    let pid = match read_pid(config_dir) {
        Some(pid) => pid,
        None => {
            cli::info("Worker not running (no PID file)");
            return Ok(());
        }
    };

    if !is_process_running(pid) {
        remove_pid_file(config_dir);
        cli::info("Worker not running (stale PID file removed)");
        return Ok(());
    }

    cli::success(&format!("Worker running (PID {pid})"));
    Ok(())
}

/// Re-exec the current binary as a detached background worker process.
#[cfg(not(tarpaulin_include))]
pub fn detach(
    config_dir: &Path,
    queues: Option<Vec<String>>,
    concurrency: Option<usize>,
    no_cron: bool,
) -> Result<()> {
    let exe = std::env::current_exe().context("Failed to determine executable path")?;

    // Warn if a worker is already running
    #[cfg(unix)]
    if let Some(pid) = read_pid(config_dir)
        && is_process_running(pid)
    {
        tracing::warn!(
            "Worker PID file exists with PID {} — another worker may be running",
            pid
        );
    }

    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let mut cmd = process::Command::new(&exe);
    cmd.arg("-C").arg(&config_dir).arg("work");

    if let Some(ref q) = queues {
        cmd.arg("--queues").arg(q.join(","));
    }
    if let Some(c) = concurrency {
        cmd.arg("--concurrency").arg(c.to_string());
    }
    if no_cron {
        cmd.arg("--no-cron");
    }

    cmd.env("_CRAP_DETACHED", "1");

    let child = cmd
        .stdin(process::Stdio::null())
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .spawn()
        .context("Failed to spawn detached worker")?;

    let pid = child.id();
    write_pid_file(&config_dir, pid)?;
    cli::success(&format!("Started worker in background (PID {})", pid));

    Ok(())
}

/// Run a standalone job worker.
#[cfg(not(tarpaulin_include))]
pub async fn run(
    config_dir: &Path,
    queues: Option<Vec<String>>,
    concurrency: Option<usize>,
    no_cron: bool,
) -> Result<()> {
    let cfg = CrapConfig::load(config_dir)?;
    cfg.validate()?;

    // Check crap_version compatibility
    if let Some(warning) = cfg.check_version() {
        tracing::warn!("{}", warning);
    }

    // Prune old log files if file logging is enabled
    if cfg.logging.file {
        let log_dir = cfg.log_dir(config_dir);
        if log_dir.exists() {
            match super::logs::prune_old_logs(&log_dir, cfg.logging.max_files) {
                Ok(0) => {}
                Ok(n) => info!("Pruned {n} old log file(s)"),
                Err(e) => tracing::warn!("Failed to prune old log files: {e}"),
            }
        }
    }

    // Initialize Lua VM and load collections/globals
    let registry = hooks::init_lua(config_dir, &cfg).context("Failed to initialize Lua VM")?;

    // Initialize database + sync schema
    let db_pool = pool::create_pool(config_dir, &cfg).context("Failed to create database pool")?;
    migrate::sync_all(&db_pool, &registry, &cfg.locale)
        .context("Failed to sync database schema")?;

    // Initialize hook runner
    let hook_runner = HookRunner::builder()
        .config_dir(config_dir)
        .registry(registry.clone())
        .config(&cfg)
        .build()?;

    // Run on_init hooks (synchronous — failure aborts startup)
    if !cfg.hooks.on_init.is_empty() {
        info!("Running on_init hooks...");
        let mut conn = db_pool.get().context("DB connection for on_init")?;
        let tx = conn.transaction().context("Transaction for on_init")?;
        hook_runner
            .run_system_hooks_with_conn(&cfg.hooks.on_init, &tx)
            .context("on_init hooks failed")?;
        tx.commit().context("Commit on_init transaction")?;
    }

    let storage = create_storage(config_dir, &cfg.upload)?;
    let email_provider = create_email_provider(&cfg.email)?;

    // PID file management
    write_pid_file(config_dir, process::id())?;
    let pid_config_dir = config_dir.to_path_buf();

    let shutdown = CancellationToken::new();

    // Two-stage shutdown: first signal → graceful, second → force exit
    {
        let shutdown_clone = shutdown.clone();
        tokio::spawn(async move {
            // First signal: graceful shutdown
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigterm =
                    signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        info!("Received SIGINT, shutting down worker gracefully...");
                    }
                    _ = sigterm.recv() => {
                        info!("Received SIGTERM, shutting down worker gracefully...");
                    }
                }
            }

            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
                info!("Received shutdown signal, shutting down worker gracefully...");
            }

            shutdown_clone.cancel();

            // Second signal: force exit
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigterm =
                    signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        tracing::warn!("Received second SIGINT, forcing exit");
                    }
                    _ = sigterm.recv() => {
                        tracing::warn!("Received second SIGTERM, forcing exit");
                    }
                }
            }

            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
                tracing::warn!("Received second shutdown signal, forcing exit");
            }

            std::process::exit(1);
        });
    }

    let mut jobs_config = cfg.jobs.clone();

    if let Some(c) = concurrency {
        jobs_config.max_concurrent = c;
    }

    if let Some(ref q) = queues {
        info!("Worker processing queues: {}", q.join(", "));
    } else {
        info!("Worker processing all queues");
    }

    if no_cron {
        info!("Cron scheduling disabled for this worker");
    }

    info!(
        "Starting worker (concurrency={}, cron={})",
        jobs_config.max_concurrent, !no_cron
    );

    scheduler::start(
        SchedulerParamsBuilder::new(
            db_pool,
            hook_runner,
            registry,
            jobs_config,
            shutdown,
            storage,
            cfg.locale.clone(),
        )
        .email_provider(email_provider)
        .email_queue_timeout(cfg.email.queue_timeout)
        .email_queue_concurrency(cfg.email.queue_concurrency)
        .build(),
    )
    .await?;

    // Clean up PID file on graceful shutdown
    remove_pid_file(&pid_config_dir);
    info!("Worker stopped");

    Ok(())
}
