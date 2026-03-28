//! `serve` command — start admin UI and gRPC servers.

use anyhow::{Context as _, Result, anyhow, bail};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process,
    sync::Arc,
};
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
use tokio::{select, try_join};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Which server to start when using `--only`.
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum ServeMode {
    Admin,
    Api,
}

use crate::{
    admin, api, cli,
    config::{AuthConfig, CrapConfig},
    core::{
        Registry, SharedRegistry, event::EventBus, rate_limit::LoginRateLimiter,
        upload::format_filesize,
    },
    db::{DbConnection, migrate, pool},
    hooks,
    hooks::HookRunner,
    scheduler, typegen,
};

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
fn check_existing_pid(config_dir: &Path) {
    let path = pid_file_path(config_dir);

    if let Ok(contents) = fs::read_to_string(&path)
        && let Ok(pid) = contents.trim().parse::<u32>()
    {
        // Check if process is still running (kill -0)
        let running = Path::new(&format!("/proc/{}", pid)).exists();

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
    check_existing_pid(&config_dir);

    let mut cmd = process::Command::new(&exe);
    cmd.arg("serve").arg(&config_dir);

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
        let secret = nanoid::nanoid!(64);
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

    // Create shared rate limiters — both admin and gRPC servers share the same
    // instances so an attacker can't double their attempt budget across servers.
    let login_limiter = Arc::new(LoginRateLimiter::new(
        cfg.auth.max_login_attempts,
        cfg.auth.login_lockout_seconds,
    ));
    let ip_login_limiter = Arc::new(LoginRateLimiter::new(
        cfg.auth.max_ip_login_attempts,
        cfg.auth.login_lockout_seconds,
    ));
    let forgot_password_limiter = Arc::new(LoginRateLimiter::new(
        cfg.auth.max_forgot_password_attempts,
        cfg.auth.forgot_password_window_seconds,
    ));
    // Uses max_ip_login_attempts intentionally — shared per-IP budget for login
    // and forgot-password (same threat model: brute-force from a single IP).
    let ip_forgot_password_limiter = Arc::new(LoginRateLimiter::new(
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
                pool.clone(),
                hook_runner.clone(),
                registry.clone(),
                cfg.jobs.clone(),
                shutdown.clone(),
                config_dir.clone(),
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
    fn check_existing_pid_no_file_no_warning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Should not panic
        check_existing_pid(tmp.path());
    }

    #[test]
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
