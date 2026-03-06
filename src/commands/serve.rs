//! `serve` command — start admin UI and gRPC servers.

use anyhow::{Context, Result};
use std::path::Path;
use tracing::{info, warn, error};

/// Re-exec the current binary as a detached background process.
#[cfg(not(tarpaulin_include))]
pub fn detach(config_dir: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("Failed to determine executable path")?;

    let child = std::process::Command::new(&exe)
        .arg("serve")
        .arg(config_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Failed to spawn detached process")?;

    println!("Started crap-cms in background (PID {})", child.id());
    Ok(())
}

/// Start the admin UI and gRPC servers.
#[cfg(not(tarpaulin_include))]
pub async fn run(config_dir: &Path) -> Result<()> {
    let config_dir = config_dir.canonicalize().unwrap_or_else(|_| config_dir.to_path_buf());

    info!("Config directory: {}", config_dir.display());

    // Load config
    let cfg = crate::config::CrapConfig::load(&config_dir)
        .context("Failed to load config")?;
    info!(?cfg, "Configuration loaded");

    // Check crap_version compatibility
    if let Some(warning) = cfg.check_version() {
        warn!("{}", warning);
    }

    // Initialize Lua VM and load collections/globals
    let registry = crate::hooks::init_lua(&config_dir, &cfg)
        .context("Failed to initialize Lua VM")?;

    {
        let reg = registry.read()
            .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
        info!("Loaded {} collection(s), {} global(s)",
            reg.collections.len(), reg.globals.len());
        for (slug, col) in &reg.collections {
            info!("  Collection '{}': {} field(s)", slug, col.fields.len());
        }
    }

    // Auto-generate Lua type definitions on startup
    {
        let reg = registry.read()
            .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
        match crate::typegen::generate(&config_dir, &reg) {
            Ok(path) => info!("Generated type definitions: {}", path.display()),
            Err(e) => warn!("Failed to generate type definitions: {}", e),
        }
    }

    // Initialize database
    let pool = crate::db::pool::create_pool(&config_dir, &cfg)
        .context("Failed to create database pool")?;

    // Sync database schema from Lua definitions
    crate::db::migrate::sync_all(&pool, &registry, &cfg.locale)
        .context("Failed to sync database schema")?;

    // Initialize Lua hook runner (with registry for CRUD access in hooks)
    let hook_runner = crate::hooks::lifecycle::HookRunner::new(&config_dir, registry.clone(), &cfg)?;

    // Run on_init hooks (synchronous — failure aborts startup)
    if !cfg.hooks.on_init.is_empty() {
        info!("Running on_init hooks...");
        let mut conn = pool.get().context("DB connection for on_init")?;
        let tx = conn.transaction().context("Transaction for on_init")?;
        hook_runner.run_system_hooks_with_conn(&cfg.hooks.on_init, &tx)
            .context("on_init hooks failed")?;
        tx.commit().context("Commit on_init transaction")?;
        info!("on_init hooks completed");
    }

    // Resolve JWT secret
    let jwt_secret = if cfg.auth.secret.is_empty() {
        // Persist auto-generated secret so sessions survive restarts
        let secret_path = config_dir.join("data").join(".jwt_secret");
        if secret_path.exists() {
            match std::fs::read_to_string(&secret_path) {
                Ok(s) if !s.trim().is_empty() => {
                    info!("Using persisted JWT secret from {}", secret_path.display());
                    s.trim().to_string()
                }
                _ => {
                    let secret = nanoid::nanoid!(64);
                    let _ = std::fs::create_dir_all(secret_path.parent().unwrap());
                    if let Err(e) = std::fs::write(&secret_path, &secret) {
                        warn!("Failed to write JWT secret to {}: {}", secret_path.display(), e);
                    } else {
                        warn!("Generated and persisted JWT secret to {}", secret_path.display());
                    }
                    secret
                }
            }
        } else {
            let secret = nanoid::nanoid!(64);
            let _ = std::fs::create_dir_all(secret_path.parent().unwrap());
            if let Err(e) = std::fs::write(&secret_path, &secret) {
                warn!("Failed to write JWT secret to {}: {}", secret_path.display(), e);
            } else {
                warn!("Generated and persisted JWT secret to {}", secret_path.display());
            }
            secret
        }
    } else {
        cfg.auth.secret.clone()
    };

    // Log auth collection info
    {
        let reg = registry.read()
            .map_err(|e| anyhow::anyhow!("Registry lock poisoned: {}", e))?;
        let auth_collections: Vec<_> = reg.collections.values()
            .filter(|d| d.is_auth_collection())
            .map(|d| d.slug.as_str())
            .collect();
        if auth_collections.is_empty() {
            info!("No auth collections — admin UI and API are open");
        } else {
            info!("Auth collections: {:?} — admin login required", auth_collections);
        }
    }

    // Snapshot the registry for hot-path consumers (admin UI + gRPC).
    // HookRunner + scheduler keep the SharedRegistry (which is only read at runtime anyway).
    let registry_snapshot = crate::core::Registry::snapshot(&registry);

    // Create EventBus for live updates (if enabled)
    let event_bus = if cfg.live.enabled {
        let bus = crate::core::event::EventBus::new(cfg.live.channel_capacity);
        info!("Live event streaming enabled (capacity: {})", cfg.live.channel_capacity);
        Some(bus)
    } else {
        info!("Live event streaming disabled");
        None
    };

    // Start servers
    let admin_addr = format!("{}:{}", cfg.server.host, cfg.server.admin_port);
    let grpc_addr = format!("{}:{}", cfg.server.host, cfg.server.grpc_port);

    info!("Starting Admin UI on http://{}", admin_addr);
    info!("Starting gRPC API on {}", grpc_addr);

    let admin_handle = crate::admin::server::start(
        &admin_addr,
        cfg.clone(),
        config_dir.clone(),
        pool.clone(),
        registry_snapshot.clone(),
        hook_runner.clone(),
        jwt_secret.clone(),
        event_bus.clone(),
    );

    let grpc_handle = crate::api::start_server(
        &grpc_addr,
        pool.clone(),
        registry_snapshot,
        hook_runner.clone(),
        jwt_secret,
        &cfg.depth,
        &cfg,
        &config_dir,
        event_bus,
    );

    // Start the background job scheduler
    let scheduler_pool = pool.clone();
    let scheduler_runner = hook_runner.clone();
    let scheduler_registry = registry.clone();
    let scheduler_config = cfg.jobs.clone();
    let scheduler_handle = async move {
        crate::scheduler::start(scheduler_pool, scheduler_runner, scheduler_registry, scheduler_config)
            .await
    };

    tokio::try_join!(admin_handle, grpc_handle, scheduler_handle)
        .map_err(|e| {
            error!("Server error: {}", e);
            e
        })?;

    Ok(())
}
