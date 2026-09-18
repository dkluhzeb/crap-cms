//! Shared helper functions used across multiple command handlers.

#[cfg(unix)]
use std::io;
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};
#[cfg(unix)]
use std::{
    thread::sleep,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, bail};
use tracing::{info, warn};

#[cfg(unix)]
use crate::config::JobsConfig;
use crate::{
    config::CrapConfig,
    core::{
        Registry, SharedEventTransport, SharedInvalidationTransport,
        event::{create_event_transport, create_invalidation_transport},
    },
    db::{DbPool, migrate, pool},
    hooks::{self, HookRunner},
};

#[cfg(unix)]
use tokio::select;
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::sync::CancellationToken;

/// An open project: its config and registry, a synced database pool, and the
/// instance lock held shared for as long as the project is open, so a restore
/// or `migrate fresh` can't replace the database underneath.
pub struct Project {
    pub config: CrapConfig,
    pub registry: Arc<Registry>,
    pub pool: DbPool,
    /// Bind it to a named variable: dropping it releases the lock.
    pub lock: InstanceLock,
}

/// Open the project for a CLI command that reads or writes data: load the
/// config, take the instance lock shared, init Lua, create the pool, and sync
/// the schema.
///
/// # Errors
///
/// Returns an error if config loading, the instance lock, Lua init, pool
/// creation, or schema sync fails.
pub fn open_project(config_dir: &Path) -> Result<Project> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let config = CrapConfig::load(&config_dir).context("Failed to load config")?;
    config.apply()?;
    if let Some(warning) = config.check_version() {
        warn!("{}", warning);
    }

    let lock = hold_instance_lock(&config_dir)?;
    let registry = hooks::init_lua(&config_dir, &config).context("Failed to initialize Lua VM")?;
    let pool = pool::create_pool(&config_dir, &config).context("Failed to create database pool")?;

    migrate::sync_all(&pool, &registry, &config.locale)
        .context("Failed to sync database schema")?;

    Ok(Project {
        config,
        registry,
        pool,
        lock,
    })
}

/// Load a config and put it into service, check version, and prune old log
/// files. Shared by serve and work commands.
pub fn load_and_validate_config(config_dir: &Path) -> Result<CrapConfig> {
    let cfg = CrapConfig::load(config_dir)?;

    // Validation plus the process-wide limits the config carries, through the
    // one chokepoint test configs go through too.
    cfg.apply()?;

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
/// Build event + invalidation transports from config. The Redis URL is shared
/// with the cache backend (same `[cache] redis_url`). Used by every process
/// that runs writes — `serve`, the standalone `work` worker, and stdio MCP —
/// so cross-process (Redis) live updates and user-invalidation reach `serve`'s
/// subscribers regardless of which process performed the write.
///
/// # Errors
///
/// Returns an error if a configured Redis transport can't be constructed.
pub fn create_live_transports(
    cfg: &CrapConfig,
) -> Result<(Option<SharedEventTransport>, SharedInvalidationTransport)> {
    let redis_url = &cfg.cache.redis_url;
    let event_transport = create_event_transport(&cfg.live, redis_url)?;
    let invalidation_transport = create_invalidation_transport(&cfg.live, redis_url)?;

    Ok((event_transport, invalidation_transport))
}

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

// ── Graceful-stop helpers ────────────────────────────────────────────────

/// How often a stop checks whether the process it signalled has exited.
#[cfg(unix)]
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long the kernel is given to reap a force-killed process before its PID
/// file is removed.
#[cfg(unix)]
const SIGKILL_SETTLE: Duration = Duration::from_millis(500);

/// Names where the stop deadline comes from, for the log line that announces
/// how long the stop will wait.
#[cfg(unix)]
pub const STOP_DEADLINE_SOURCE: &str = "longest configured job timeout plus drain grace";

/// How long a `--stop` waits for a detached crap-cms process to drain before
/// it escalates to `SIGKILL`.
///
/// Every crap-cms process drains on `SIGTERM`: it stops taking new work and
/// waits for what it already holds — the job runs it claimed, and for a server
/// the requests in flight. Killing it before those can finish is the very
/// crash the drain exists to avoid, so the deadline comes from the same
/// configured job timeouts the process itself drains by. A config that won't
/// load falls back to the framework defaults — never to a number below them.
#[cfg(unix)]
pub fn stop_deadline(config_dir: &Path) -> Duration {
    let secs = load_and_validate_config(config_dir).map_or_else(
        |e| {
            warn!("Could not read job timeouts for the stop deadline ({e:#}) — using defaults");

            JobsConfig::default().drain_deadline_secs()
        },
        |cfg| cfg.jobs.drain_deadline_secs(),
    );

    Duration::from_secs(secs)
}

/// Poll `predicate` until it returns `false` or the timeout elapses.
///
/// Returns `true` iff the predicate transitioned to `false` before the
/// deadline, `false` iff the timeout elapsed first. The predicate is called
/// once immediately, then at each `poll_interval` until the deadline.
#[cfg(unix)]
fn wait_until_false<F>(timeout: Duration, poll_interval: Duration, mut predicate: F) -> bool
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        if !predicate() {
            return true;
        }

        sleep(poll_interval);
    }

    !predicate()
}

/// Wait up to `grace` for `pid` to exit. `true` iff it exited in time.
#[cfg(unix)]
pub fn wait_for_exit(pid: u32, grace: Duration) -> bool {
    wait_until_false(grace, STOP_POLL_INTERVAL, || is_process_running(pid))
}

/// Force-kill a process that outlasted its stop deadline, then give the kernel
/// a moment to reap it so the caller's PID-file cleanup is not racing it.
#[cfg(unix)]
pub fn force_kill(pid: u32) {
    let _ = send_signal(pid, libc::SIGKILL);

    sleep(SIGKILL_SETTLE);
}

/// The server's PID file name (written by `serve`, read by `serve --stop` and
/// the status checks).
pub const SERVER_PID_FILENAME: &str = "crap.pid";

/// The lock file of a project's data directory. Every process that opens the
/// database — `serve`, `work`, stdio `mcp` and CLI commands — holds it shared;
/// a destructive database command takes it exclusively.
pub const INSTANCE_LOCK_FILENAME: &str = "crap.lock";

/// A held instance lock, released when dropped.
#[derive(Debug)]
#[must_use = "the instance lock is released as soon as this is dropped"]
pub struct InstanceLock {
    _file: File,
}

/// Open the lock file of a project's data directory, creating it when missing.
/// A shared lock takes an existing file through a read-only handle — it needs
/// no write access, so a read-only data directory works once the file exists.
/// An exclusive lock opens it for writing: on NFS, and wherever file locks are
/// `fcntl` byte-range locks, an exclusive lock needs a descriptor open for
/// writing.
fn open_instance_lock(config_dir: &Path, exclusive: bool) -> Result<File> {
    let path = pid_file_path(config_dir, INSTANCE_LOCK_FILENAME);
    let context = || format!("Failed to open the instance lock: {}", path.display());

    if !exclusive {
        match File::open(&path) {
            Ok(file) => return Ok(file),
            Err(e) if e.kind() != ErrorKind::NotFound => return Err(e).with_context(context),
            Err(_) => {}
        }
    }

    let _ = fs::create_dir_all(path.parent().expect("lock path has parent"));

    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(context)
}

/// Hold the instance lock shared for as long as the returned lock lives, so a
/// destructive database command can't run while this process uses the
/// database. Take it before opening the database: a process that opens it
/// first could write to a database a restore is replacing.
///
/// # Errors
///
/// Returns an error when a destructive database command holds the lock, or the
/// lock file can't be opened or locked — a filesystem without file locks can't
/// keep a restore and a running process apart.
pub fn hold_instance_lock(config_dir: &Path) -> Result<InstanceLock> {
    let file = open_instance_lock(config_dir, false)?;

    match file.try_lock_shared() {
        Ok(()) => Ok(InstanceLock { _file: file }),
        Err(TryLockError::WouldBlock) => bail!(
            "a database restore or `migrate fresh` is running on this project — start again \
             once it has finished"
        ),
        Err(TryLockError::Error(e)) => Err(e).context("Failed to take the instance lock"),
    }
}

/// Take the instance lock exclusively for a destructive database command,
/// refusing while any other crap-cms process — `serve`, `work`, stdio `mcp` or a
/// CLI command — uses the project: replacing the database under an open pool
/// leaves that process writing to the old file. Keep the returned lock for the
/// whole command, so none of them can start meanwhile.
///
/// # Errors
///
/// Returns an error when another process holds the instance lock, or the lock
/// file can't be opened or locked.
pub fn hold_exclusive_instance_lock(config_dir: &Path, command: &str) -> Result<InstanceLock> {
    let file = open_instance_lock(config_dir, true)?;

    match file.try_lock() {
        Ok(()) => Ok(InstanceLock { _file: file }),
        Err(TryLockError::WouldBlock) => bail!(
            "`{command}` refused: another crap-cms process (a server, worker, MCP process or CLI \
             command) is using this project. Stop it first — an open pool would keep writing to \
             the old database file."
        ),
        Err(TryLockError::Error(e)) => Err(e).context("Failed to take the instance lock"),
    }
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

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;
    #[cfg(unix)]
    use std::sync::atomic::{AtomicU32, Ordering};

    #[cfg(unix)]
    use crate::config::JOB_DRAIN_GRACE_SECS;

    use super::*;

    /// The escalation to `SIGKILL` must never come before the longest job a
    /// process in this deployment is allowed to run.
    #[cfg(unix)]
    #[test]
    fn the_stop_deadline_tracks_the_configured_job_timeouts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(
            tmp.path().join("crap.toml"),
            "[jobs.queues.reports]\ntimeout = \"3h\"\n",
        )
        .unwrap();

        assert_eq!(
            stop_deadline(tmp.path()),
            Duration::from_secs(3 * 3600 + JOB_DRAIN_GRACE_SECS)
        );
    }

    /// A config that can't be read must not silently shrink the deadline to
    /// something below the framework's own job timeouts.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_config_falls_back_to_the_framework_deadline() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "[jobs]\nnot_a_field = 1\n").unwrap();

        assert_eq!(
            stop_deadline(tmp.path()),
            Duration::from_secs(JobsConfig::default().drain_deadline_secs())
        );
    }

    #[cfg(unix)]
    #[test]
    fn wait_until_false_returns_true_when_predicate_flips() {
        let counter = AtomicU32::new(0);
        let exited = wait_until_false(Duration::from_secs(5), Duration::from_millis(10), || {
            counter.fetch_add(1, Ordering::SeqCst) < 3
        });
        assert!(exited, "predicate flipped false, should return true");
    }

    /// A process that never responds to `SIGTERM` must let the deadline
    /// elapse, so the caller escalates to `SIGKILL`. The test uses a short
    /// timeout so it runs quickly.
    #[cfg(unix)]
    #[test]
    fn wait_until_false_returns_false_when_predicate_stays_true() {
        let exited = wait_until_false(
            Duration::from_millis(100),
            Duration::from_millis(10),
            || true,
        );
        assert!(!exited, "predicate never flipped, should return false");
    }

    /// Regression: CLI commands opened the database without the instance lock,
    /// so a restore could replace the database while one wrote to it.
    #[test]
    fn an_open_project_keeps_a_restore_out() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("crap.toml"), "").unwrap();

        let project = open_project(tmp.path()).unwrap();
        assert!(hold_exclusive_instance_lock(tmp.path(), "restore").is_err());

        drop(project);
        assert!(hold_exclusive_instance_lock(tmp.path(), "restore").is_ok());
    }

    /// Regression: the lock file was always opened for writing, so every CLI
    /// command failed on a read-only data directory. The shared lock takes an
    /// existing lock file through a read-only handle.
    #[cfg(unix)]
    #[test]
    fn an_existing_lock_file_takes_the_shared_lock_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        drop(hold_instance_lock(tmp.path()).unwrap());

        let lock_path = pid_file_path(tmp.path(), INSTANCE_LOCK_FILENAME);
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o444)).unwrap();

        let serving = hold_instance_lock(tmp.path()).unwrap();
        let listing = hold_instance_lock(tmp.path()).unwrap();
        drop((serving, listing));
    }

    /// A destructive command is refused while a serving process holds the
    /// instance lock, and a serving process can't start while it runs.
    #[test]
    fn instance_lock_keeps_restore_and_serving_processes_apart() {
        let tmp = tempfile::tempdir().unwrap();

        let serving = hold_instance_lock(tmp.path()).unwrap();
        let err = hold_exclusive_instance_lock(tmp.path(), "restore").unwrap_err();
        assert!(err.to_string().contains("refused"), "{err}");
        drop(serving);

        let restoring = hold_exclusive_instance_lock(tmp.path(), "restore").unwrap();
        assert!(hold_instance_lock(tmp.path()).is_err());
        drop(restoring);

        assert!(hold_instance_lock(tmp.path()).is_ok());
    }

    #[test]
    fn pid_file_path_lives_under_the_data_subdir() {
        assert_eq!(
            pid_file_path(Path::new("/app"), "server.pid"),
            PathBuf::from("/app/data/server.pid")
        );
    }
}
