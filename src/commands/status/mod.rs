//! `status` command — show project status (collections, globals, migrations, jobs, uploads).
//!
//! With `--check`, runs a best-practice audit on the configuration and project state.

pub(crate) mod check;
mod display;

use anyhow::{Context as _, Result, anyhow};
use std::path::Path;

use crate::{
    cli,
    config::CrapConfig,
    db::{migrate, pool},
    hooks,
};

/// Print project status: collections, globals, migrations, jobs, uploads, locale.
/// With `run_check = true`, also runs a best-practice audit.
pub fn run(config_dir: &Path, run_check: bool) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;
    let registry = hooks::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;
    let pool = pool::create_pool(&config_dir, &cfg).context("Failed to create database pool")?;

    migrate::sync_all(&pool, &registry, &cfg.locale).context("Failed to sync database schema")?;

    let reg = registry
        .read()
        .map_err(|e| anyhow!("Registry lock poisoned: {}", e))?;

    let conn = pool.get().context("Failed to get database connection")?;

    cli::header("Project Status");
    cli::kv("Config", &config_dir.display().to_string());
    display::print_db_info(&cfg, &config_dir, &conn);
    display::print_uploads_info(&config_dir);
    display::print_locale_info(&cfg);
    display::print_server_info(&cfg);

    println!();
    display::print_collections(&reg, &conn);

    println!();
    display::print_globals(&reg);

    println!();
    display::print_versions(&reg);

    println!();
    display::print_access(&cfg, &reg);

    println!();
    display::print_hooks(&reg);

    println!();
    display::print_live(&cfg, &reg);

    println!();
    display::print_migrations(&config_dir, &pool);
    display::print_jobs(&reg, &conn, &config_dir);

    if run_check {
        check::run_checks(&cfg, &reg, &conn, &pool, &config_dir);
    }

    Ok(())
}
