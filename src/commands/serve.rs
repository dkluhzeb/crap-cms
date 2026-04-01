//! `serve` command — start admin UI and gRPC servers.

use anyhow::{Context as _, Result, anyhow, bail};
use nanoid::nanoid;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
#[cfg(unix)]
use tokio::select;
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio::try_join;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::{
    admin, api, cli,
    config::{AuthConfig, CrapConfig},
    core::{
        Registry, SharedRegistry,
        cache::create_cache,
        email::create_email_provider,
        event::EventBus,
        rate_limit::{LoginRateLimiter, create_rate_limit_backend},
        upload::{create_storage, format_filesize},
    },
    db::{DbConnection, migrate, pool},
    hooks,
    hooks::HookRunner,
    scheduler, typegen,
};

/// Which server to start when using `--only`.
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum ServeMode {
    Admin,
    Api,
}

/// Send a signal to a process by PID.
#[cfg(unix)]
fn send_signal(pid: u32, sig: i32) -> Result<()> {
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
/// Uses `kill(pid, 0)` which checks process existence without sending a signal.
/// Works across all Unix platforms (not just Linux with /proc).
#[cfg(unix)]
fn is_process_running(pid: u32) -> bool {
    let Ok(pid_i32) = i32::try_from(pid) else {
        return false;
    };
    unsafe { libc::kill(pid_i32, 0) == 0 }
}

/// Bail early if the config directory doesn't look valid.
fn validate_config_dir(config_dir: &Path) -> Result<()> {
    if !config_dir.join("crap.toml").exists() {
        bail!(
            "No crap.toml found in '{}'. Is this a valid config directory?",
            config_dir.display()
        );
    }

    Ok(())
}

/// Path to the PID file within the config directory.
fn pid_file_path(config_dir: &Path) -> PathBuf {
    config_dir.join("data").join("crap.pid")
}

/// Write the current process PID to the PID file.
fn write_pid_file(config_dir: &Path, pid: u32) -> Result<()> {
    let path = pid_file_path(config_dir);
    let _ = fs::create_dir_all(path.parent().expect("pid path has parent"));

    fs::write(&path, pid.to_string())
        .with_context(|| format!("Failed to write PID file: {}", path.display()))?;

    Ok(())
}

/// Remove the PID file on clean shutdown.
fn remove_pid_file(config_dir: &Path) {
    let path = pid_file_path(config_dir);

    if path.exists() {
        let _ = fs::remove_file(&path);
    }
}

/// Check if a PID file exists and warn if the process is still running.
#[cfg(unix)]
fn check_existing_pid(config_dir: &Path) {
    let path = pid_file_path(config_dir);

    if let Ok(contents) = fs::read_to_string(&path)
        && let Ok(pid) = contents.trim().parse::<u32>()
    {
        let running = is_process_running(pid);

        if running {
            warn!(
                "PID file exists with PID {} — another instance may be running",
                pid
            );
        }
    }
}

/// Re-exec the current binary as a detached background process.
#[cfg(not(tarpaulin_include))]
pub fn detach(config_dir: &Path, only: Option<ServeMode>, no_scheduler: bool) -> Result<()> {
    let exe = env::current_exe().context("Failed to determine executable path")?;

    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    validate_config_dir(&config_dir)?;

    #[cfg(unix)]
    check_existing_pid(&config_dir);

    let mut cmd = process::Command::new(&exe);

    cmd.arg("-C").arg(&config_dir).arg("serve");

    if let Some(mode) = only {
        cmd.arg("--only");
        cmd.arg(match mode {
            ServeMode::Admin => "admin",
            ServeMode::Api => "api",
        });
    }

    if no_scheduler {
        cmd.arg("--no-scheduler");
    }

    // Tell the child it was detached so it can auto-enable file logging
    // (the child runs without --detach, so it can't detect this itself).
    cmd.env("_CRAP_DETACHED", "1");

    let child = cmd
        .stdin(process::Stdio::null())
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .spawn()
        .context("Failed to spawn detached process")?;

    let pid = child.id();

    write_pid_file(&config_dir, pid)?;

    cli::success(&format!("Started crap-cms in background (PID {})", pid));

    Ok(())
}

/// Read the PID from the PID file. Returns `None` if no file or not parseable.
#[cfg(unix)]
fn read_pid(config_dir: &Path) -> Option<u32> {
    let path = pid_file_path(config_dir);

    fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

/// Stop a running detached instance by sending SIGTERM, falling back to SIGKILL.
#[cfg(unix)]
pub fn stop(config_dir: &Path) -> Result<()> {
    validate_config_dir(config_dir)?;

    let pid = read_pid(config_dir).context(
        "No PID file found — is there a detached instance running?\n\
         Start one with: crap-cms serve --detach",
    )?;

    if !is_process_running(pid) {
        remove_pid_file(config_dir);

        bail!("Process {} is not running (stale PID file removed)", pid);
    }

    // Send SIGTERM for graceful shutdown.
    send_signal(pid, libc::SIGTERM)?;

    // Wait for graceful shutdown (up to 10 seconds).
    let deadline = Instant::now() + Duration::from_secs(10);

    while Instant::now() < deadline {
        if !is_process_running(pid) {
            remove_pid_file(config_dir);
            cli::success(&format!("Stopped crap-cms (PID {pid})"));

            return Ok(());
        }

        thread::sleep(Duration::from_millis(100));
    }

    // Still running — force kill.
    cli::warning(&format!(
        "Process {pid} did not stop within 10s, sending SIGKILL"
    ));

    let _ = send_signal(pid, libc::SIGKILL);

    // Brief wait for the force kill to take effect.
    thread::sleep(Duration::from_millis(500));
    remove_pid_file(config_dir);
    cli::success(&format!("Force-stopped crap-cms (PID {pid})"));

    Ok(())
}

/// Restart a detached instance: stop the current one, then start a new one.
#[cfg(unix)]
pub fn restart(config_dir: &Path, only: Option<ServeMode>, no_scheduler: bool) -> Result<()> {
    validate_config_dir(config_dir)?;

    // Stop if running — tolerate "not running" errors (race between check and kill).
    if let Some(pid) = read_pid(config_dir) {
        if is_process_running(pid) {
            if let Err(e) = stop(config_dir) {
                // Process may have exited between check and stop — not an error.
                debug!("stop() during restart: {e}");
            }
        } else {
            remove_pid_file(config_dir);
        }
    }

    detach(config_dir, only, no_scheduler)
}

/// Show the status of a detached instance.
#[cfg(unix)]
pub fn status(config_dir: &Path) -> Result<()> {
    validate_config_dir(config_dir)?;

    let pid = match read_pid(config_dir) {
        Some(pid) => pid,
        None => {
            cli::info("Not running (no PID file)");

            return Ok(());
        }
    };

    if !is_process_running(pid) {
        remove_pid_file(config_dir);
        cli::info("Not running (stale PID file removed)");

        return Ok(());
    }

    cli::success(&format!("Running (PID {pid})"));

    // Try to show uptime from /proc on Linux.
    #[cfg(target_os = "linux")]
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        show_uptime(&stat);
    }

    Ok(())
}

/// Parse process start time from /proc/[pid]/stat and print uptime.
///
/// Uses `/proc/uptime` for system uptime and `/proc/[pid]/stat` field 22
/// (starttime in clock ticks). CLK_TCK is read from `getconf CLK_TCK`.
#[cfg(target_os = "linux")]
fn show_uptime(stat: &str) {
    // Field 22 is starttime in clock ticks since boot.
    // Fields after ") " (skipping pid and comm which may contain spaces).
    let fields: Vec<&str> = stat
        .rsplit(')')
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect();

    // Field 22 is at index 19 in the post-comm fields.
    let start_ticks: u64 = match fields.get(19).and_then(|s| s.parse().ok()) {
        Some(v) => v,
        None => return,
    };

    // Get CLK_TCK via getconf (avoids libc dependency).
    let clk_tck: u64 = process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(100); // 100 is the default on Linux

    let uptime_str = match fs::read_to_string("/proc/uptime") {
        Ok(s) => s,
        Err(_) => return,
    };

    let system_uptime_secs: f64 = match uptime_str
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
    {
        Some(v) => v,
        None => return,
    };

    let process_start_secs = start_ticks as f64 / clk_tck as f64;
    let uptime_secs = (system_uptime_secs - process_start_secs).max(0.0) as u64;

    cli::kv("Uptime", &format_duration(uptime_secs));
}

/// Format seconds into a human-readable duration string.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn format_duration(secs: u64) -> String {
    let days = secs / 86400;
    let hours = (secs % 86400) / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if days > 0 {
        format!("{days}d {hours}h {minutes}m {seconds}s")
    } else if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// Resolve the JWT secret: load from file, generate + persist, or use config value.
fn resolve_jwt_secret(auth_cfg: &AuthConfig, config_dir: &Path) -> Result<String> {
    if auth_cfg.secret.is_empty() {
        let secret_path = config_dir.join("data").join(".jwt_secret");

        // Try loading existing secret
        if let Ok(s) = fs::read_to_string(&secret_path)
            && !s.trim().is_empty()
        {
            debug!("Using persisted JWT secret from {}", secret_path.display());
            return Ok(s.trim().to_string());
        }

        // Generate and persist a new secret
        let secret = nanoid!(64);
        let _ = fs::create_dir_all(secret_path.parent().expect("path has parent"));

        fs::write(&secret_path, &secret).with_context(|| {
            format!(
                "Failed to persist JWT secret to {} — cannot start with ephemeral secret \
                 (all sessions would be lost on restart)",
                secret_path.display()
            )
        })?;

        // Restrict file permissions to owner-only on Unix
        #[cfg(unix)]
        if let Err(e) = fs::set_permissions(&secret_path, fs::Permissions::from_mode(0o600)) {
            warn!(
                "Failed to set permissions on JWT secret file {}: {}",
                secret_path.display(),
                e
            );
        }

        warn!(
            "Generated and persisted JWT secret to {}",
            secret_path.display()
        );

        Ok(secret)
    } else {
        Ok(auth_cfg.secret.clone().into_inner())
    }
}

/// Spawn a task that listens for shutdown signals (SIGINT/SIGTERM) and cancels the token.
fn spawn_shutdown_signal(shutdown: CancellationToken) {
    tokio::spawn(async move {
        // First signal: graceful shutdown
        #[cfg(unix)]
        {
            let mut sigterm =
                signal(SignalKind::terminate()).expect("Failed to register SIGTERM handler");
            select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("Received SIGINT, shutting down gracefully...");
                }
                _ = sigterm.recv() => {
                    info!("Received SIGTERM, shutting down gracefully...");
                }
            }
        }

        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            info!("Received shutdown signal, shutting down gracefully...");
        }

        shutdown.cancel();

        // Second signal: force exit
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

        process::exit(1);
    });
}

/// Log information about loaded collections, type definitions, and auth status.
fn log_startup_info(registry: &SharedRegistry, cfg: &CrapConfig) -> Result<()> {
    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let auth_collections: Vec<_> = reg
        .collections
        .values()
        .filter(|d| d.is_auth_collection())
        .map(|d| &*d.slug)
        .collect();

    if auth_collections.is_empty() {
        info!("No auth collections — admin UI and API are open");
    } else {
        info!(
            "Auth collections: {:?} — admin login required",
            auth_collections
        );
    }

    // Warn about per-collection max_file_size exceeding the global body limit
    let global_max = cfg.upload.max_file_size;
    for (slug, def) in &reg.collections {
        if let Some(ref upload_cfg) = def.upload
            && let Some(collection_max) = upload_cfg.max_file_size
            && collection_max > global_max
        {
            warn!(
                "Collection '{}' has max_file_size ({}) exceeding global limit ({}). \
                         Axum's body limit will reject uploads before the per-collection check.",
                slug,
                format_filesize(collection_max),
                format_filesize(global_max),
            );
        }
    }

    Ok(())
}

/// Start the admin UI and gRPC servers.
#[cfg(not(tarpaulin_include))]
pub async fn run(config_dir: &Path, only: Option<ServeMode>, no_scheduler: bool) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());
    validate_config_dir(&config_dir)?;

    // PID file management
    #[cfg(unix)]
    check_existing_pid(&config_dir);
    write_pid_file(&config_dir, process::id())?;

    info!("Config directory: {}", config_dir.display());

    // Load config
    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;
    info!("Configuration loaded");

    // Validate configuration
    cfg.validate().context("Invalid configuration")?;

    // Check crap_version compatibility
    if let Some(warning) = cfg.check_version() {
        warn!("{}", warning);
    }

    // Prune old log files if file logging is enabled
    if cfg.logging.file {
        let log_dir = cfg.log_dir(&config_dir);
        if log_dir.exists() {
            match super::logs::prune_old_logs(&log_dir, cfg.logging.max_files) {
                Ok(0) => {}
                Ok(n) => info!("Pruned {n} old log file(s)"),
                Err(e) => warn!("Failed to prune old log files: {e}"),
            }
        }
    }

    // Initialize Lua VM and load collections/globals
    let registry = hooks::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;

    {
        let reg = registry
            .read()
            .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

        info!(
            "Loaded {} collection(s), {} global(s)",
            reg.collections.len(),
            reg.globals.len()
        );

        for (slug, col) in &reg.collections {
            info!("  Collection '{}': {} field(s)", slug, col.fields.len());
        }
    }

    // Auto-generate Lua type definitions on startup
    {
        let reg = registry
            .read()
            .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

        match typegen::generate(&config_dir, &reg) {
            Ok(path) => info!("Generated type definitions: {}", path.display()),
            Err(e) => warn!("Failed to generate type definitions: {}", e),
        }
    }

    // Initialize database
    let pool = pool::create_pool(&config_dir, &cfg).context("Failed to create database pool")?;

    // Sync database schema from Lua definitions
    migrate::sync_all(&pool, &registry, &cfg.locale).context("Failed to sync database schema")?;

    // Initialize Lua hook runner (with registry for CRUD access in hooks)
    let hook_runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry.clone())
        .config(&cfg)
        .build()?;

    // Run on_init hooks (synchronous — failure aborts startup)
    if !cfg.hooks.on_init.is_empty() {
        info!("Running on_init hooks...");
        let mut conn = pool.get().context("DB connection for on_init")?;
        let tx = conn.transaction().context("Transaction for on_init")?;

        hook_runner
            .run_system_hooks_with_conn(&cfg.hooks.on_init, &tx)
            .context("on_init hooks failed")?;

        tx.commit().context("Commit on_init transaction")?;

        info!("on_init hooks completed");
    }

    // Resolve JWT secret
    let jwt_secret = resolve_jwt_secret(&cfg.auth, &config_dir)?;

    // Log auth collection info and validate upload limits
    log_startup_info(&registry, &cfg)?;

    // Security warnings for common misconfigurations
    // NOTE: MCP HTTP without API key is now caught in CrapConfig::validate()
    if cfg.server.grpc_rate_limit_requests == 0 {
        warn!("gRPC API rate limiting is disabled (grpc_rate_limit_requests = 0)");
    }
    if cfg.server.h2c && !cfg.server.trust_proxy {
        warn!(
            "h2c enabled but trust_proxy is false — \
             per-IP rate limiting will use the proxy's IP, not the client's"
        );
    }
    if !cfg.email.smtp_host.is_empty() && cfg.server.public_url.is_none() {
        warn!(
            "Email is configured (smtp_host set) but server.public_url is not set — \
             password reset links will use http://{}:{} which may not be reachable externally",
            cfg.server.host, cfg.server.admin_port
        );
    }

    // Snapshot the registry for hot-path consumers (admin UI + gRPC).
    // HookRunner + scheduler keep the SharedRegistry (which is only read at runtime anyway).
    let registry_snapshot = Registry::snapshot(&registry);

    // Create EventBus for live updates (if enabled)
    let event_bus = if cfg.live.enabled {
        let bus = EventBus::new(cfg.live.channel_capacity);

        info!(
            "Live event streaming enabled (capacity: {})",
            cfg.live.channel_capacity
        );

        Some(bus)
    } else {
        info!("Live event streaming disabled");

        None
    };

    // Create upload storage backend
    let storage = create_storage(&config_dir, &cfg.upload)?;

    // Create cache backend
    let cache = create_cache(&cfg.cache)?;

    // Graceful shutdown: CancellationToken shared across all servers
    let shutdown = CancellationToken::new();
    spawn_shutdown_signal(shutdown.clone());

    // Determine which components to start
    let run_admin = only.is_none() || matches!(only, Some(ServeMode::Admin));
    let run_api = only.is_none() || matches!(only, Some(ServeMode::Api));
    let run_scheduler = !no_scheduler;

    // Start servers
    let admin_addr = format!("{}:{}", cfg.server.host, cfg.server.admin_port);
    let grpc_addr = format!("{}:{}", cfg.server.host, cfg.server.grpc_port);

    if run_admin {
        info!("Starting Admin UI on http://{}", admin_addr);
    }
    if run_api {
        info!("Starting gRPC API on {}", grpc_addr);
    }
    if !run_scheduler {
        info!("Background job scheduler disabled");
    }

    // Create rate limit backend
    let rl_redis_url = if cfg.auth.rate_limit_redis_url.is_empty() {
        &cfg.cache.redis_url
    } else {
        &cfg.auth.rate_limit_redis_url
    };
    let rl_backend = create_rate_limit_backend(
        &cfg.auth.rate_limit_backend,
        rl_redis_url,
        &cfg.auth.rate_limit_prefix,
    )?;

    // Create shared rate limiters — both admin and gRPC servers share the same
    // instances so an attacker can't double their attempt budget across servers.
    let login_limiter = Arc::new(LoginRateLimiter::with_backend(
        rl_backend.clone(),
        "login",
        cfg.auth.max_login_attempts,
        cfg.auth.login_lockout_seconds,
    ));
    let ip_login_limiter = Arc::new(LoginRateLimiter::with_backend(
        rl_backend.clone(),
        "ip_login",
        cfg.auth.max_ip_login_attempts,
        cfg.auth.login_lockout_seconds,
    ));
    let forgot_password_limiter = Arc::new(LoginRateLimiter::with_backend(
        rl_backend.clone(),
        "forgot",
        cfg.auth.max_forgot_password_attempts,
        cfg.auth.forgot_password_window_seconds,
    ));
    // Uses max_ip_login_attempts intentionally — shared per-IP budget for login
    // and forgot-password (same threat model: brute-force from a single IP).
    let ip_forgot_password_limiter = Arc::new(LoginRateLimiter::with_backend(
        rl_backend.clone(),
        "ip_forgot",
        cfg.auth.max_ip_login_attempts,
        cfg.auth.forgot_password_window_seconds,
    ));

    let admin_handle = async {
        if run_admin {
            admin::server::start(
                &admin_addr,
                admin::server::AdminStartParams::builder()
                    .config(cfg.clone())
                    .config_dir(config_dir.clone())
                    .pool(pool.clone())
                    .registry(registry_snapshot.clone())
                    .hook_runner(hook_runner.clone())
                    .jwt_secret(jwt_secret.clone())
                    .event_bus(event_bus.clone())
                    .login_limiter(login_limiter.clone())
                    .ip_login_limiter(ip_login_limiter.clone())
                    .forgot_password_limiter(forgot_password_limiter.clone())
                    .ip_forgot_password_limiter(ip_forgot_password_limiter.clone())
                    .storage(storage.clone())
                    .build(),
                shutdown.clone(),
            )
            .await
        } else {
            Ok(())
        }
    };

    let grpc_handle = async {
        if run_api {
            api::server::start(
                &grpc_addr,
                api::server::GrpcStartParams::builder()
                    .pool(pool.clone())
                    .registry(registry_snapshot.clone())
                    .hook_runner(hook_runner.clone())
                    .jwt_secret(jwt_secret.clone())
                    .config(cfg.clone())
                    .config_dir(config_dir.clone())
                    .event_bus(event_bus.clone())
                    .login_limiter(login_limiter.clone())
                    .ip_login_limiter(ip_login_limiter.clone())
                    .forgot_password_limiter(forgot_password_limiter.clone())
                    .ip_forgot_password_limiter(ip_forgot_password_limiter.clone())
                    .storage(storage.clone())
                    .cache(cache.clone())
                    .rate_limit_backend(rl_backend)
                    .build(),
                shutdown.clone(),
            )
            .await
        } else {
            Ok(())
        }
    };

    let scheduler_handle = async {
        if run_scheduler {
            scheduler::start(
                scheduler::SchedulerParamsBuilder::new(
                    pool.clone(),
                    hook_runner.clone(),
                    registry.clone(),
                    cfg.jobs.clone(),
                    shutdown.clone(),
                    storage.clone(),
                    cfg.locale.clone(),
                )
                .email_provider(create_email_provider(&cfg.email)?)
                .email_queue_timeout(cfg.email.queue_timeout)
                .email_queue_concurrency(cfg.email.queue_concurrency)
                .build(),
            )
            .await
        } else {
            Ok(())
        }
    };

    try_join!(admin_handle, grpc_handle, scheduler_handle).map_err(|e| {
        error!("Server error: {}", e);
        e
    })?;

    remove_pid_file(&config_dir);

    // Checkpoint WAL before exit — process::exit() skips destructors
    if let Ok(conn) = pool.get()
        && conn.kind() == "sqlite"
        && let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
    {
        warn!("WAL checkpoint failed: {}", e);
    }

    info!("All servers stopped. Goodbye.");

    // Force-exit: the tokio runtime's blocking pool shutdown waits indefinitely
    // for any lingering spawn_blocking threads (e.g. image processing, Lua hooks).
    // All business logic is complete at this point — let the OS reclaim resources.
    process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_file_write_and_remove() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path();

        write_pid_file(config_dir, 12345).unwrap();

        let path = pid_file_path(config_dir);
        assert!(path.exists());
        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents, "12345");

        remove_pid_file(config_dir);
        assert!(!path.exists());
    }

    #[test]
    fn pid_file_path_is_in_data_dir() {
        let path = pid_file_path(Path::new("/some/config"));
        assert_eq!(path, PathBuf::from("/some/config/data/crap.pid"));
    }

    #[test]
    fn remove_pid_file_noop_if_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Should not panic
        remove_pid_file(tmp.path());
    }

    #[test]
    #[cfg(unix)]
    fn check_existing_pid_no_file_no_warning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Should not panic
        check_existing_pid(tmp.path());
    }

    #[test]
    #[cfg(unix)]
    fn check_existing_pid_stale_pid_no_panic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Write a PID that almost certainly doesn't exist
        write_pid_file(tmp.path(), 999999999).unwrap();
        // Should not panic
        check_existing_pid(tmp.path());
    }

    #[test]
    fn validate_config_dir_missing_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = validate_config_dir(tmp.path()).unwrap_err();
        assert!(
            err.to_string().contains("No crap.toml found"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn validate_config_dir_with_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        validate_config_dir(tmp.path()).unwrap();
    }

    #[test]
    fn resolve_jwt_secret_generates_and_persists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let auth = AuthConfig::default(); // secret is empty

        let secret = resolve_jwt_secret(&auth, tmp.path()).unwrap();
        assert!(!secret.is_empty());

        // Secret file must exist on disk
        let secret_path = tmp.path().join("data").join(".jwt_secret");
        assert!(secret_path.exists());

        let persisted = fs::read_to_string(&secret_path).unwrap();
        assert_eq!(persisted, secret);
    }

    #[test]
    fn resolve_jwt_secret_reuses_persisted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let auth = AuthConfig::default();

        let first = resolve_jwt_secret(&auth, tmp.path()).unwrap();
        let second = resolve_jwt_secret(&auth, tmp.path()).unwrap();

        assert_eq!(first, second, "Must reuse persisted secret across calls");
    }

    #[test]
    fn resolve_jwt_secret_uses_config_value() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let auth = AuthConfig {
            secret: crate::core::JwtSecret::new("my-explicit-secret"),
            ..Default::default()
        };

        let secret = resolve_jwt_secret(&auth, tmp.path()).unwrap();
        assert_eq!(secret, "my-explicit-secret");

        // No file should be written when config provides the secret
        let secret_path = tmp.path().join("data").join(".jwt_secret");
        assert!(!secret_path.exists());
    }

    #[test]
    #[cfg(unix)]
    fn read_pid_no_file_returns_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert!(read_pid(tmp.path()).is_none());
    }

    #[test]
    #[cfg(unix)]
    fn read_pid_valid_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_pid_file(tmp.path(), 42).unwrap();
        assert_eq!(read_pid(tmp.path()), Some(42));
    }

    #[test]
    #[cfg(unix)]
    fn read_pid_garbage_returns_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = pid_file_path(tmp.path());
        let _ = fs::create_dir_all(path.parent().unwrap());
        fs::write(&path, "not-a-number").unwrap();
        assert!(read_pid(tmp.path()).is_none());
    }

    #[test]
    #[cfg(unix)]
    fn is_process_running_current_pid() {
        assert!(is_process_running(process::id()));
    }

    #[test]
    #[cfg(unix)]
    fn is_process_running_bogus_pid() {
        assert!(!is_process_running(999_999_999));
    }

    #[test]
    #[cfg(unix)]
    fn is_process_running_u32_max_returns_false() {
        // Regression: u32::MAX (4294967295) exceeds i32::MAX and previously
        // could wrap to a negative PID via `as i32`, potentially matching
        // a real process group. The fix uses i32::try_from() which returns
        // false for out-of-range values instead of wrapping.
        assert!(
            !is_process_running(u32::MAX),
            "u32::MAX should not be treated as a valid PID"
        );
    }

    #[test]
    fn stop_no_pid_file_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        let err = stop(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("No PID file"));
    }

    #[test]
    fn stop_stale_pid_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        write_pid_file(tmp.path(), 999_999_999).unwrap();

        let err = stop(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("not running"));
        // PID file should be cleaned up
        assert!(!pid_file_path(tmp.path()).exists());
    }

    #[test]
    fn status_no_pid_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        // Should not error — just prints "Not running"
        status(tmp.path()).unwrap();
    }

    #[test]
    fn status_stale_pid_cleans_up() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("crap.toml"), "").unwrap();
        write_pid_file(tmp.path(), 999_999_999).unwrap();

        status(tmp.path()).unwrap();
        // Stale PID file should be removed
        assert!(!pid_file_path(tmp.path()).exists());
    }

    #[test]
    fn format_duration_seconds_only() {
        assert_eq!(format_duration(45), "45s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(125), "2m 5s");
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(format_duration(3661), "1h 1m 1s");
    }

    #[test]
    fn format_duration_days() {
        assert_eq!(format_duration(90061), "1d 1h 1m 1s");
    }

    #[test]
    fn resolve_jwt_secret_fails_on_unwritable_path() {
        // Regression: previously, a write failure would silently return an
        // ephemeral secret, causing session loss on restart.
        let err = resolve_jwt_secret(&AuthConfig::default(), Path::new("/nonexistent/path"));
        assert!(err.is_err());

        let msg = err.unwrap_err().to_string();
        assert!(
            msg.contains("cannot start with ephemeral secret"),
            "unexpected error: {}",
            msg
        );
    }
}
