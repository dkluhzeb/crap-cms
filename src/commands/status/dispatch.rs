//! `status` command entry point — load the project and print the
//! status sections, optionally running the best-practice audit.

use std::path::Path;

use anyhow::{Context as _, Result};

use crate::{
    cli,
    commands::{Project, open_project},
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

    let Project {
        lock: _instance_lock,
        config: cfg,
        registry,
        pool,
    } = open_project(&config_dir)?;

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
        let warnings = check::run_checks(&cfg, &registry, &conn, &pool, &config_dir);

        // CI usability: an audit that found problems must be
        // distinguishable from a clean one by exit code. Mirrors
        // `update check` exiting 1 when an update is available.
        if warnings > 0 {
            std::process::exit(2);
        }
    }

    Ok(())
}
