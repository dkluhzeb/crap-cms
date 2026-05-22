//! Server startup — config loading, initialization, and server orchestration.

use anyhow::{Context as _, Result, anyhow, bail};
use chrono::Utc;
use nanoid::nanoid;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{fs, path::Path, process, sync::Arc};
use tokio::try_join;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::{
    admin, api,
    commands::{
        helpers::{load_and_validate_config, run_on_init_hooks, spawn_shutdown_signal},
        update,
    },
    config::{AuthConfig, CrapConfig},
    core::{
        Registry, SharedEventTransport, SharedInvalidationTransport, SharedPasswordProvider,
        SharedTokenProvider,
        auth::{Argon2PasswordProvider, JwtTokenProvider},
        cache::create_cache,
        email::create_email_provider,
        event::{create_event_transport, create_invalidation_transport},
        rate_limit::{
            LoginRateLimiter, RateLimitBackend, RateLimitFactoryConfig, create_rate_limit_backend,
        },
        upload::{create_storage, format_filesize},
    },
    db::{DbConnection, DbPool, SharedPopulateSingleflight, Singleflight, migrate, pool},
    hooks::{self, HookRunner},
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
fn log_startup_info(registry: &Registry, cfg: &CrapConfig) {
    let auth_collections: Vec<_> = registry
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

    for (slug, def) in &registry.collections {
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
}

/// Print a one-line nudge at startup if the cached update-check shows a
/// newer release than the running binary. Reads from
/// `$XDG_CACHE_HOME/crap-cms/update-check.json` (populated by
/// `crap-cms update check`, 24h TTL). Never performs network I/O.
fn log_update_notice(cfg: &CrapConfig) {
    if !cfg.update.check_on_startup {
        return;
    }

    let Some(path) = update::cache::default_path() else {
        return;
    };
    let Some(latest) = update::cache::fresh_latest_at(&path, Utc::now()) else {
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

/// Log a nudge if health checks find issues.
fn log_health_check_nudge(cfg: &CrapConfig, registry: &Registry, pool: &DbPool, config_dir: &Path) {
    let Ok(conn) = pool.get() else {
        return;
    };

    let n = crate::commands::status::check::count_warnings(cfg, registry, &conn, pool, config_dir);

    if n > 0 {
        warn!("{n} health check warning(s) found — run `crap-cms status --check` for details.");
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
fn init_lua_and_typegen(config_dir: &Path, cfg: &CrapConfig) -> Result<Arc<Registry>> {
    let registry = hooks::init_lua(config_dir, cfg).context("Failed to initialize Lua VM")?;

    let n_hooks: usize = registry
        .collections
        .values()
        .map(|c| {
            c.hooks.before_validate.len()
                + c.hooks.before_change.len()
                + c.hooks.after_change.len()
                + c.hooks.before_read.len()
                + c.hooks.after_read.len()
                + c.hooks.before_delete.len()
                + c.hooks.after_delete.len()
                + c.hooks.before_broadcast.len()
        })
        .sum();

    info!(
        "Loaded {} collection(s), {} global(s), {} hook(s)",
        registry.collections.len(),
        registry.globals.len(),
        n_hooks,
    );

    for (slug, col) in &registry.collections {
        debug!("  Collection '{}': {} field(s)", slug, col.fields.len());
    }

    // Auto-regenerate server-side Lua types in dev_mode so hook authors
    // see updated `crap.lua` / `hooks.lua` after editing collections
    // without remembering to run `crap-cms typegen lua`. In production
    // we skip the I/O entirely — types are an explicit user action via
    // `crap-cms typegen lua`, and surprise file writes on every startup
    // are undesirable. Failures are warn-only — a missing/un-writeable
    // types dir should never block the server.
    if cfg.admin.dev_mode {
        typegen::generate_lua(config_dir, &registry, None)
            .inspect(|paths| {
                for path in paths {
                    info!("Regenerated Lua types: {}", path.display());
                }
            })
            .inspect_err(|e| warn!("Failed to regenerate Lua types: {}", e))
            .ok();
    }

    Ok(registry)
}

/// All rate limiters created during startup.
struct RateLimiters {
    login: Arc<LoginRateLimiter>,
    ip_login: Arc<LoginRateLimiter>,
    forgot_password: Arc<LoginRateLimiter>,
    ip_forgot_password: Arc<LoginRateLimiter>,
    backend: Arc<dyn RateLimitBackend>,
}

/// Create shared rate limiters for login and forgot-password flows.
fn create_rate_limiters(cfg: &CrapConfig) -> Result<RateLimiters> {
    let rl_redis_url = if cfg.auth.rate_limit_redis_url.is_empty() {
        &cfg.cache.redis_url
    } else {
        &cfg.auth.rate_limit_redis_url
    };

    let backend = create_rate_limit_backend(&RateLimitFactoryConfig {
        backend: &cfg.auth.rate_limit_backend,
        redis_url: rl_redis_url,
        prefix: &cfg.auth.rate_limit_prefix,
    })?;

    let login = Arc::new(LoginRateLimiter::with_backend(
        backend.clone(),
        "login",
        cfg.auth.max_login_attempts,
        cfg.auth.login_lockout_seconds,
    ));

    let ip_login = Arc::new(LoginRateLimiter::with_backend(
        backend.clone(),
        "ip_login",
        cfg.auth.max_ip_login_attempts,
        cfg.auth.login_lockout_seconds,
    ));

    let forgot_password = Arc::new(LoginRateLimiter::with_backend(
        backend.clone(),
        "forgot",
        cfg.auth.max_forgot_password_attempts,
        cfg.auth.forgot_password_window_seconds,
    ));

    // Uses max_ip_login_attempts intentionally — shared per-IP budget for login
    // and forgot-password (same threat model: brute-force from a single IP).
    let ip_forgot_password = Arc::new(LoginRateLimiter::with_backend(
        backend.clone(),
        "ip_forgot",
        cfg.auth.max_ip_login_attempts,
        cfg.auth.forgot_password_window_seconds,
    ));

    Ok(RateLimiters {
        login,
        ip_login,
        forgot_password,
        ip_forgot_password,
        backend,
    })
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
    i32::from(!cleanup_errors.is_empty())
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
                errors.push(anyhow!("WAL checkpoint failed: {e}"));
            }
        }
        Ok(_) => {}
        Err(e) => {
            warn!(
                "Failed to obtain DB connection for shutdown checkpoint: {}",
                e
            );
            errors.push(anyhow!("shutdown DB connection: {e}"));
        }
    }

    info!("All servers stopped. Goodbye.");

    errors
}

/// All shared resources created during startup. Owned bundle threaded
/// through the per-server task spawners (`run_admin_task`,
/// `run_grpc_task`, `run_scheduler_task`) so the orchestrator stays a
/// thin chain of phase calls.
struct StartupResources {
    config: CrapConfig,
    config_dir: std::path::PathBuf,
    pool: DbPool,
    registry: Arc<Registry>,
    hook_runner: HookRunner,
    jwt_secret: String,
    event_transport: Option<SharedEventTransport>,
    invalidation_transport: SharedInvalidationTransport,
    populate_singleflight: SharedPopulateSingleflight,
    storage: crate::core::SharedStorage,
    cache: crate::core::SharedCache,
    token_provider: SharedTokenProvider,
    password_provider: SharedPasswordProvider,
    rate_limiters: RateLimiters,
}

/// Phase 1–3 of startup: load + validate config, init Lua/typegen, open
/// the DB pool, run migrations, build the live event/invalidation
/// transports, construct the `HookRunner`, run `on_init` hooks, resolve
/// the JWT secret, create the shared singletons (storage, cache,
/// auth providers, rate limiters), and emit the startup-info logs.
///
/// Caller must have already canonicalized `config_dir`, validated it,
/// written the PID file, and checked for an existing process — those
/// happen before this call so the PID file isn't created for invalid
/// configs.
#[cfg(not(tarpaulin_include))]
fn bootstrap_startup(config_dir: std::path::PathBuf) -> Result<StartupResources> {
    let config = load_and_validate_config(&config_dir)?;
    info!("Configuration loaded");
    let registry = init_lua_and_typegen(&config_dir, &config)?;
    let pool = pool::create_pool(&config_dir, &config).context("Failed to create database pool")?;

    migrate::sync_all(&pool, &registry, &config.locale)
        .context("Failed to sync database schema")?;

    // Build the live transports first so the HookRunner VMs can carry the
    // invalidation transport as app_data — this lets Lua delete/lock paths
    // publish user-invalidation signals through the service layer.
    let (event_transport, invalidation_transport) = create_live_transports(&config)?;

    // Process-wide populate singleflight, shared by both the gRPC
    // ContentService and the HookRunner VMs so cache-miss fetches dedup
    // across concurrent requests (regardless of whether the originating
    // call tree starts in gRPC or in a Lua hook). The service layer's
    // access-leak guardrail discards this Arc for override-access callers
    // (MCP, Lua `opts.overrideAccess = true`).
    let populate_singleflight: SharedPopulateSingleflight = Arc::new(Singleflight::new());

    let hook_runner = HookRunner::builder()
        .config_dir(&config_dir)
        .registry(Arc::clone(&registry))
        .config(&config)
        .invalidation_transport(invalidation_transport.clone())
        .populate_singleflight(populate_singleflight.clone())
        .build()?;

    run_on_init_hooks(&config, &pool, &hook_runner)?;

    let jwt_secret = resolve_jwt_secret(&config.auth, &config_dir)?;
    log_startup_info(&registry, &config);
    log_security_warnings(&config);
    log_update_notice(&config);
    log_health_check_nudge(&config, &registry, &pool, &config_dir);

    let storage = create_storage(&config_dir, &config.upload)?;
    let cache = create_cache(&config.cache)?;
    let token_provider: SharedTokenProvider = Arc::new(JwtTokenProvider::new(&jwt_secret));
    let password_provider: SharedPasswordProvider = Arc::new(Argon2PasswordProvider);
    let rate_limiters = create_rate_limiters(&config)?;

    Ok(StartupResources {
        config,
        config_dir,
        pool,
        registry,
        hook_runner,
        jwt_secret,
        event_transport,
        invalidation_transport,
        populate_singleflight,
        storage,
        cache,
        token_provider,
        password_provider,
        rate_limiters,
    })
}

/// Build the admin server's start-params from the shared startup resources.
#[cfg(not(tarpaulin_include))]
fn build_admin_params(res: &StartupResources) -> admin::server::AdminStartParams {
    admin::server::AdminStartParams::builder()
        .config(res.config.clone())
        .config_dir(res.config_dir.clone())
        .pool(res.pool.clone())
        .registry(Arc::clone(&res.registry))
        .hook_runner(res.hook_runner.clone())
        .jwt_secret(res.jwt_secret.clone())
        .event_transport(res.event_transport.clone())
        .login_limiter(res.rate_limiters.login.clone())
        .ip_login_limiter(res.rate_limiters.ip_login.clone())
        .forgot_password_limiter(res.rate_limiters.forgot_password.clone())
        .ip_forgot_password_limiter(res.rate_limiters.ip_forgot_password.clone())
        .storage(res.storage.clone())
        .token_provider(res.token_provider.clone())
        .password_provider(res.password_provider.clone())
        .invalidation_transport(res.invalidation_transport.clone())
        .cache(Some(res.cache.clone()))
        .build()
}

/// Build the gRPC server's start-params from the shared startup resources.
#[cfg(not(tarpaulin_include))]
fn build_grpc_params(res: &StartupResources) -> api::server::GrpcStartParams {
    api::server::GrpcStartParams::builder()
        .pool(res.pool.clone())
        .registry(Arc::clone(&res.registry))
        .hook_runner(res.hook_runner.clone())
        .config(res.config.clone())
        .config_dir(res.config_dir.clone())
        .event_transport(res.event_transport.clone())
        .login_limiter(res.rate_limiters.login.clone())
        .ip_login_limiter(res.rate_limiters.ip_login.clone())
        .forgot_password_limiter(res.rate_limiters.forgot_password.clone())
        .ip_forgot_password_limiter(res.rate_limiters.ip_forgot_password.clone())
        .storage(res.storage.clone())
        .cache(res.cache.clone())
        .token_provider(res.token_provider.clone())
        .password_provider(res.password_provider.clone())
        .rate_limit_backend(res.rate_limiters.backend.clone())
        .invalidation_transport(res.invalidation_transport.clone())
        .populate_singleflight(res.populate_singleflight.clone())
        .build()
}

/// Run the admin server (no-op when `enabled = false`).
#[cfg(not(tarpaulin_include))]
async fn run_admin_task(
    res: &StartupResources,
    addr: &str,
    enabled: bool,
    shutdown: CancellationToken,
) -> Result<()> {
    if !enabled {
        return Ok(());
    }
    admin::server::start(addr, build_admin_params(res), shutdown).await
}

/// Run the gRPC server (no-op when `enabled = false`).
#[cfg(not(tarpaulin_include))]
async fn run_grpc_task(
    res: &StartupResources,
    addr: &str,
    enabled: bool,
    shutdown: CancellationToken,
) -> Result<()> {
    if !enabled {
        return Ok(());
    }
    api::server::start(addr, build_grpc_params(res), shutdown).await
}

/// Run the background-job scheduler (no-op when `enabled = false`).
#[cfg(not(tarpaulin_include))]
async fn run_scheduler_task(
    res: &StartupResources,
    enabled: bool,
    shutdown: CancellationToken,
) -> Result<()> {
    if !enabled {
        return Ok(());
    }
    scheduler::start(scheduler::SchedulerParams {
        pool: res.pool.clone(),
        hook_runner: res.hook_runner.clone(),
        registry: Arc::clone(&res.registry),
        config: res.config.jobs.clone(),
        shutdown,
        storage: res.storage.clone(),
        locale_config: res.config.locale.clone(),
        email_provider: Some(create_email_provider(&res.config.email)?),
        email_queue_timeout: res.config.email.queue_timeout,
        email_queue_concurrency: res.config.email.queue_concurrency,
    })
    .await
}

/// Start the admin UI and gRPC servers.
///
/// # Errors
///
/// Returns an error if config validation, PID file write, server startup,
/// or graceful shutdown fails.
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

    let res = bootstrap_startup(config_dir)?;

    let shutdown = CancellationToken::new();
    spawn_shutdown_signal(shutdown.clone(), "");

    let run_admin = only.is_none() || matches!(only, Some(ServeMode::Admin));
    let run_api = only.is_none() || matches!(only, Some(ServeMode::Api));
    let run_scheduler_flag = !no_scheduler;

    let admin_addr = format!(
        "{}:{}",
        res.config.server.host, res.config.server.admin_port
    );
    let grpc_addr = format!("{}:{}", res.config.server.host, res.config.server.grpc_port);

    log_component_status(
        run_admin,
        run_api,
        run_scheduler_flag,
        &admin_addr,
        &grpc_addr,
    );

    try_join!(
        run_admin_task(&res, &admin_addr, run_admin, shutdown.clone()),
        run_grpc_task(&res, &grpc_addr, run_api, shutdown.clone()),
        run_scheduler_task(&res, run_scheduler_flag, shutdown.clone()),
    )
    .inspect_err(|e| error!("Server error: {}", e))?;

    let cleanup_errors = shutdown_cleanup(&res.config_dir, &res.pool);
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
            "unexpected error: {err}"
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
            "unexpected error: {msg}"
        );
    }
}
