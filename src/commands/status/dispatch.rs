//! `status` command entry point — load the project and print the
//! status sections, optionally running the best-practice audit.

use std::path::Path;

use anyhow::{Context as _, Result};

use crate::{
    cli,
    config::CrapConfig,
    db::{migrate, pool},
    hooks,
};

use super::{check, display};

/// Print project status: collections, globals, migrations, jobs, uploads, locale.
/// With `run_check = true`, also runs a best-practice audit.
///
/// # Errors
///
/// Returns an error if config loading, Lua init, pool creation, or
/// schema sync fails.
pub fn run(config_dir: &Path, run_check: bool) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;
    let registry = hooks::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;
    let pool = pool::create_pool(&config_dir, &cfg).context("Failed to create database pool")?;

    migrate::sync_all(&pool, &registry, &cfg.locale).context("Failed to sync database schema")?;

    let conn = pool.get().context("Failed to get database connection")?;

    cli::header("Project Status");
    cli::kv("Config", &config_dir.display().to_string());
    display::print_db_info(&cfg, &config_dir, &conn);
    display::print_uploads_info(&config_dir);
    display::print_customizations(&config_dir);
    display::print_locale_info(&cfg);
    display::print_server_info(&cfg);

    println!();
    display::print_collections(&registry, &conn);

    println!();
    display::print_globals(&registry);

    println!();
    display::print_versions(&registry);

    println!();
    display::print_access(&cfg, &registry);

    println!();
    display::print_hooks(&registry);

    println!();
    display::print_live(&cfg, &registry);

    println!();
    display::print_migrations(&config_dir, &pool);
    display::print_jobs(&registry, &conn, &config_dir);

    if run_check {
        check::run_checks(&cfg, &registry, &conn, &pool, &config_dir);
    }

    Ok(())
}
