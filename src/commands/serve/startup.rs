//! Server startup — config loading, initialization, and server orchestration.

use anyhow::{Context as _, Result, anyhow, bail};
use nanoid::nanoid;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{fs, path::Path, process, sync::Arc};
use tokio::try_join;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::{
    admin, api,
    commands::helpers::{load_and_validate_config, run_on_init_hooks, spawn_shutdown_signal},
    config::{AuthConfig, CrapConfig},
    core::{
        Registry, SharedRegistry,
        auth::{
            Argon2PasswordProvider, JwtTokenProvider, SharedPasswordProvider, SharedTokenProvider,
        },
        cache::create_cache,
        email::create_email_provider,
        event::{
            SharedEventTransport, SharedInvalidationTransport, create_event_transport,
            create_invalidation_transport,
        },
        rate_limit::{LoginRateLimiter, RateLimitBackend, create_rate_limit_backend},
        upload::{create_storage, format_filesize},
    },
    db::{
        DbConnection, DbPool, migrate, pool,
        query::{SharedPopulateSingleflight, Singleflight},
    },
    hooks,
    hooks::HookRunner,
    scheduler, typegen,
};

#[cfg(unix)]
use super::pid::check_existing_pid;
use super::pid::{remove_pid_file, write_pid_file};

/// Which server to start when using `--only`.
#[derive(Clone, Copy, clap::ValueEnum)]
pub enum ServeMode {
    Admin,
    Api,
}

/// Bail early if the config directory doesn't look valid.
pub fn validate_config_dir(config_dir: &Path) -> Result<()> {
    if !config_dir.join("crap.toml").exists() {
        bail!(
            "No crap.toml found in '{}'. Is this a valid config directory?",
            config_dir.display()
        );
    }

    Ok(())
}

/// Resolve the JWT secret: load from file, generate + persist, or use config value.
fn resolve_jwt_secret(auth_cfg: &AuthConfig, config_dir: &Path) -> Result<String> {
    if auth_cfg.secret.is_empty() {
        return resolve_jwt_secret_from_file(config_dir);
    }

    Ok(auth_cfg.secret.clone().into_inner())
}

/// Generate or load a persisted JWT secret from the data directory.
fn resolve_jwt_secret_from_file(config_dir: &Path) -> Result<String> {
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

/// Print a one-line nudge at startup if the cached update-check shows a
/// newer release than the running binary. Reads from
/// `$XDG_CACHE_HOME/crap-cms/update-check.json` (populated by
/// `crap-cms update check`, 24h TTL). Never performs network I/O.
fn log_update_notice(cfg: &CrapConfig) {
    if !cfg.update.check_on_startup {
        return;
    }

    let Some(path) = crate::commands::update::cache::default_path() else {
        return;
    };
    let Some(latest) = crate::commands::update::cache::fresh_latest_at(&path, chrono::Utc::now())
    else {
        return;
    };

    let current = format!("v{}", env!("CARGO_PKG_VERSION"));
    if latest != current {
        info!(
            "A newer crap-cms is available ({latest}, current {current}). \
             Run `crap-cms update` to upgrade."
        );
    }
}

/// Log security warnings for common misconfigurations.
fn log_security_warnings(cfg: &CrapConfig) {
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
}

/// Initialize Lua VM, log loaded collections, and generate type definitions.
fn init_lua_and_typegen(config_dir: &Path, cfg: &CrapConfig) -> Result<SharedRegistry> {
    let registry = hooks::init_lua(config_dir, cfg).context("Failed to initialize Lua VM")?;

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

        match typegen::generate(config_dir, &reg) {
            Ok(path) => info!("Generated type definitions: {}", path.display()),
            Err(e) => warn!("Failed to generate type definitions: {}", e),
        }
    }

    Ok(registry)
}

/// All rate limiters created during startup.
type RateLimiters = (
    Arc<LoginRateLimiter>,
    Arc<LoginRateLimiter>,
    Arc<LoginRateLimiter>,
    Arc<LoginRateLimiter>,
    Arc<dyn RateLimitBackend>,
);

/// Create shared rate limiters for login and forgot-password flows.
fn create_rate_limiters(cfg: &CrapConfig) -> Result<RateLimiters> {
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

    Ok((
        login_limiter,
        ip_login_limiter,
        forgot_password_limiter,
        ip_forgot_password_limiter,
        rl_backend,
    ))
}

/// Build event + invalidation transports from config. The Redis URL is shared
/// with the cache backend (same `[cache] redis_url`).
fn create_live_transports(
    cfg: &CrapConfig,
) -> Result<(Option<SharedEventTransport>, SharedInvalidationTransport)> {
    let redis_url = &cfg.cache.redis_url;
    let event_transport = create_event_transport(&cfg.live, redis_url)?;
    let invalidation_transport = create_invalidation_transport(&cfg.live, redis_url)?;

    Ok((event_transport, invalidation_transport))
}

/// Log which components will start based on the serve mode.
fn log_component_status(
    run_admin: bool,
    run_api: bool,
    run_scheduler: bool,
    admin_addr: &str,
    grpc_addr: &str,
) {
    if run_admin {
        info!("Starting Admin UI on http://{}", admin_addr);
    }

    if run_api {
        info!("Starting gRPC API on {}", grpc_addr);
    }

    if !run_scheduler {
        info!("Background job scheduler disabled");
    }
}

/// Compute the process exit code for the shutdown path.
///
/// Returns `0` iff every cleanup step succeeded; `1` if any step errored.
/// Orchestrators like Kubernetes rely on this to distinguish graceful shutdown
/// from partial-failure shutdown (e.g. a WAL checkpoint that never ran).
pub(crate) fn compute_shutdown_exit_code(cleanup_errors: &[anyhow::Error]) -> i32 {
    if cleanup_errors.is_empty() { 0 } else { 1 }
}

/// Perform post-shutdown cleanup: WAL checkpoint, PID file removal.
/// Returns the errors encountered so the caller can select the exit code.
fn shutdown_cleanup(config_dir: &Path, pool: &DbPool) -> Vec<anyhow::Error> {
    let mut errors: Vec<anyhow::Error> = Vec::new();

    remove_pid_file(config_dir);

    // Checkpoint WAL before exit — process::exit() skips destructors.
    match pool.get() {
        Ok(conn) if conn.kind() == "sqlite" => {
            if let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
                warn!("WAL checkpoint failed: {}", e);
                errors.push(anyhow!("WAL checkpoint failed: {}", e));
            }
        }
        Ok(_) => {}
        Err(e) => {
            warn!(
                "Failed to obtain DB connection for shutdown checkpoint: {}",
                e
            );
            errors.push(anyhow!("shutdown DB connection: {}", e));
        }
    }

    info!("All servers stopped. Goodbye.");

    errors
}

/// Start the admin UI and gRPC servers.
#[cfg(not(tarpaulin_include))]
pub async fn run(config_dir: &Path, only: Option<ServeMode>, no_scheduler: bool) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());
    validate_config_dir(&config_dir)?;

    #[cfg(unix)]
    check_existing_pid(&config_dir);
    write_pid_file(&config_dir, process::id())?;
    info!("Config directory: {}", config_dir.display());

    // Load config, init Lua, database, hooks
    let cfg = load_and_validate_config(&config_dir)?;
    info!("Configuration loaded");
    let registry = init_lua_and_typegen(&config_dir, &cfg)?;
    let pool = pool::create_pool(&config_dir, &cfg).context("Failed to create database pool")?;

    migrate::sync_all(&pool, &registry, &cfg.locale).context("Failed to sync database schema")?;

    // Build the live transports first so the HookRunner VMs can carry the
    // invalidation transport as app_data — this lets Lua delete/lock paths
    // publish user-invalidation signals through the service layer.
    let (event_transport, invalidation_transport) = create_live_transports(&cfg)?;

    // Process-wide populate singleflight, shared by both the gRPC
    // ContentService and the HookRunner VMs so cache-miss fetches dedup
    // across concurrent requests (regardless of whether the originating
    // call tree starts in gRPC or in a Lua hook). The service layer's
    // access-leak guardrail discards this Arc for override-access callers
    // (MCP, Lua `opts.overrideAccess = true`).
    let populate_singleflight: SharedPopulateSingleflight = Arc::new(Singleflight::new());

    let hook_runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(registry.clone())
        .config(&cfg)
        .invalidation_transport(invalidation_transport.clone())
        .populate_singleflight(populate_singleflight.clone())
        .build()?;

    run_on_init_hooks(&cfg, &pool, &hook_runner)?;

    // Resolve auth and create shared resources
    let jwt_secret = resolve_jwt_secret(&cfg.auth, &config_dir)?;
    log_startup_info(&registry, &cfg)?;
    log_security_warnings(&cfg);
    log_update_notice(&cfg);

    let registry_snapshot = Registry::snapshot(&registry);
    let storage = create_storage(&config_dir, &cfg.upload)?;
    let cache = create_cache(&cfg.cache)?;
    let token_provider: SharedTokenProvider = Arc::new(JwtTokenProvider::new(&jwt_secret));
    let password_provider: SharedPasswordProvider = Arc::new(Argon2PasswordProvider);

    // Shutdown signal + component selection
    let shutdown = CancellationToken::new();
    spawn_shutdown_signal(shutdown.clone(), "");

    let run_admin = only.is_none() || matches!(only, Some(ServeMode::Admin));
    let run_api = only.is_none() || matches!(only, Some(ServeMode::Api));
    let run_scheduler = !no_scheduler;

    let admin_addr = format!("{}:{}", cfg.server.host, cfg.server.admin_port);
    let grpc_addr = format!("{}:{}", cfg.server.host, cfg.server.grpc_port);

    log_component_status(run_admin, run_api, run_scheduler, &admin_addr, &grpc_addr);

    let (
        login_limiter,
        ip_login_limiter,
        forgot_password_limiter,
        ip_forgot_password_limiter,
        rl_backend,
    ) = create_rate_limiters(&cfg)?;

    // Start servers concurrently
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
                    .event_transport(event_transport.clone())
                    .login_limiter(login_limiter.clone())
                    .ip_login_limiter(ip_login_limiter.clone())
                    .forgot_password_limiter(forgot_password_limiter.clone())
                    .ip_forgot_password_limiter(ip_forgot_password_limiter.clone())
                    .storage(storage.clone())
                    .token_provider(token_provider.clone())
                    .password_provider(password_provider.clone())
                    .invalidation_transport(invalidation_transport.clone())
                    .cache(Some(cache.clone()))
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
                    .event_transport(event_transport.clone())
                    .login_limiter(login_limiter.clone())
                    .ip_login_limiter(ip_login_limiter.clone())
                    .forgot_password_limiter(forgot_password_limiter.clone())
                    .ip_forgot_password_limiter(ip_forgot_password_limiter.clone())
                    .storage(storage.clone())
                    .cache(cache.clone())
                    .token_provider(token_provider.clone())
                    .password_provider(password_provider.clone())
                    .rate_limit_backend(rl_backend)
                    .invalidation_transport(invalidation_transport.clone())
                    .populate_singleflight(populate_singleflight.clone())
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

    let cleanup_errors = shutdown_cleanup(&config_dir, &pool);
    let exit_code = compute_shutdown_exit_code(&cleanup_errors);

    // Force-exit: the tokio runtime's blocking pool shutdown waits indefinitely
    // for any lingering spawn_blocking threads (e.g. image processing, Lua hooks).
    // All business logic is complete at this point — let the OS reclaim resources.
    process::exit(exit_code);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::JwtSecret;

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
            secret: JwtSecret::new("my-explicit-secret"),
            ..Default::default()
        };

        let secret = resolve_jwt_secret(&auth, tmp.path()).unwrap();
        assert_eq!(secret, "my-explicit-secret");

        // No file should be written when config provides the secret
        let secret_path = tmp.path().join("data").join(".jwt_secret");
        assert!(!secret_path.exists());
    }

    #[test]
    fn compute_shutdown_exit_code_empty_is_zero() {
        assert_eq!(compute_shutdown_exit_code(&[]), 0);
    }

    #[test]
    fn compute_shutdown_exit_code_one_err_is_one() {
        let errs = vec![anyhow!("WAL checkpoint failed")];
        assert_eq!(compute_shutdown_exit_code(&errs), 1);
    }

    #[test]
    fn compute_shutdown_exit_code_many_errs_is_one() {
        let errs = vec![anyhow!("WAL checkpoint failed"), anyhow!("pool closed")];
        assert_eq!(compute_shutdown_exit_code(&errs), 1);
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
