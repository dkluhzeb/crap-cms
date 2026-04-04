//! `work` command — standalone job worker that processes queues without HTTP/gRPC servers.
//!
//! Used in multi-server deployments where app servers run `serve --no-scheduler`
//! and one or more dedicated workers run `work`.

use std::{
    path::Path,
    process,
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    cli,
    commands::helpers::{
        self, is_process_running, load_and_validate_config, read_pid, run_on_init_hooks,
        spawn_shutdown_signal,
    },
    core::{email::create_email_provider, upload::create_storage},
    db::{migrate, pool},
    hooks::{self, HookRunner},
    scheduler::{self, SchedulerParamsBuilder},
};

/// Worker PID filename (separate from server's crap.pid).
const PID_FILENAME: &str = "crap-worker.pid";

/// Stop a running detached worker.
#[cfg(unix)]
pub fn stop(config_dir: &Path) -> Result<()> {
    let pid = read_pid(config_dir, PID_FILENAME).context(
        "No worker PID file found — is there a detached worker running?\n\
         Start one with: crap-cms work --detach",
    )?;

    if !is_process_running(pid) {
        helpers::remove_pid_file(config_dir, PID_FILENAME);

        bail!(
            "Worker process {} is not running (stale PID file removed)",
            pid
        );
    }

    unsafe { libc::kill(i32::try_from(pid).unwrap(), libc::SIGTERM) };

    let deadline = Instant::now() + Duration::from_secs(10);

    while Instant::now() < deadline {
        if !is_process_running(pid) {
            helpers::remove_pid_file(config_dir, PID_FILENAME);

            cli::success(&format!("Stopped worker (PID {pid})"));

            return Ok(());
        }

        sleep(Duration::from_millis(100));
    }

    cli::warning(&format!(
        "Worker {pid} did not stop within 10s, sending SIGKILL"
    ));

    unsafe { libc::kill(i32::try_from(pid).unwrap(), libc::SIGKILL) };

    sleep(Duration::from_millis(500));

    helpers::remove_pid_file(config_dir, PID_FILENAME);

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
    if let Some(pid) = read_pid(config_dir, PID_FILENAME) {
        if is_process_running(pid) {
            if let Err(e) = stop(config_dir) {
                debug!("stop() during restart: {e}");
            }
        } else {
            helpers::remove_pid_file(config_dir, PID_FILENAME);
        }
    }

    detach(config_dir, queues, concurrency, no_cron)
}

/// Show status of a detached worker.
#[cfg(unix)]
pub fn status(config_dir: &Path) -> Result<()> {
    let pid = match read_pid(config_dir, PID_FILENAME) {
        Some(pid) => pid,
        None => {
            cli::info("Worker not running (no PID file)");

            return Ok(());
        }
    };

    if !is_process_running(pid) {
        helpers::remove_pid_file(config_dir, PID_FILENAME);

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
    if let Some(pid) = read_pid(config_dir, PID_FILENAME)
        && is_process_running(pid)
    {
        warn!(
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

    helpers::write_pid_file(&config_dir, PID_FILENAME, pid)?;

    cli::success(&format!("Started worker in background (PID {})", pid));

    Ok(())
}

/// Log worker configuration before starting.
fn log_worker_config(queues: &Option<Vec<String>>, no_cron: bool, concurrency: usize) {
    if let Some(q) = queues {
        info!("Worker processing queues: {}", q.join(", "));
    } else {
        info!("Worker processing all queues");
    }

    if no_cron {
        info!("Cron scheduling disabled for this worker");
    }

    info!(
        "Starting worker (concurrency={}, cron={})",
        concurrency, !no_cron
    );
}

/// Run a standalone job worker.
#[cfg(not(tarpaulin_include))]
pub async fn run(
    config_dir: &Path,
    queues: Option<Vec<String>>,
    concurrency: Option<usize>,
    no_cron: bool,
) -> Result<()> {
    let cfg = load_and_validate_config(config_dir)?;

    let registry = hooks::init_lua(config_dir, &cfg).context("Failed to initialize Lua VM")?;
    let db_pool = pool::create_pool(config_dir, &cfg).context("Failed to create database pool")?;

    migrate::sync_all(&db_pool, &registry, &cfg.locale)
        .context("Failed to sync database schema")?;

    let hook_runner = HookRunner::builder()
        .config_dir(config_dir)
        .registry(registry.clone())
        .config(&cfg)
        .build()?;

    run_on_init_hooks(&cfg, &db_pool, &hook_runner)?;

    let storage = create_storage(config_dir, &cfg.upload)?;
    let email_provider = create_email_provider(&cfg.email)?;

    helpers::write_pid_file(config_dir, PID_FILENAME, process::id())?;
    let pid_config_dir = config_dir.to_path_buf();

    let shutdown = CancellationToken::new();
    spawn_shutdown_signal(shutdown.clone(), "worker");

    let mut jobs_config = cfg.jobs.clone();

    if let Some(c) = concurrency {
        jobs_config.max_concurrent = c;
    }

    log_worker_config(&queues, no_cron, jobs_config.max_concurrent);

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

    helpers::remove_pid_file(&pid_config_dir, PID_FILENAME);
    info!("Worker stopped");

    Ok(())
}
