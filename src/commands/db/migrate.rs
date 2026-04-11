//! `migrate` subcommand: schema sync, Lua data migrations, rollback, fresh.

use std::{
    path::Path,
    sync::{Arc, RwLock},
};

use anyhow::{Context as _, Result, bail};

use crate::{
    cli::{self, Spinner, Table},
    commands::MigrateAction,
    config::CrapConfig,
    core::Registry,
    db::{DbPool, migrate as db_migrate, pool},
    hooks,
    hooks::HookRunner,
    scaffold,
};

/// Handle the `migrate` subcommand — dispatches to the appropriate action handler.
#[cfg(not(tarpaulin_include))]
pub fn migrate(config_dir: &Path, action: MigrateAction) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    if let MigrateAction::Create { ref name } = action {
        return scaffold::make_migration(&config_dir, name);
    }

    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;
    let registry = hooks::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;
    let pool = pool::create_pool(&config_dir, &cfg).context("Failed to create database pool")?;

    match action {
        MigrateAction::Create { .. } => unreachable!(),
        MigrateAction::Up => migrate_up(&config_dir, &cfg, &registry, &pool),
        MigrateAction::Down { steps } => migrate_down(&config_dir, &cfg, &registry, &pool, steps),
        MigrateAction::List => migrate_list(&config_dir, &pool),
        MigrateAction::Fresh { confirm } => {
            migrate_fresh(&config_dir, &cfg, &registry, &pool, confirm)
        }
    }
}

/// Sync schema from Lua definitions and apply pending Lua data migrations.
#[cfg(not(tarpaulin_include))]
fn migrate_up(
    config_dir: &Path,
    cfg: &CrapConfig,
    registry: &Arc<RwLock<Registry>>,
    pool: &DbPool,
) -> Result<()> {
    let spin = Spinner::new("Syncing schema...");

    db_migrate::sync_all(pool, registry, &cfg.locale).context("Failed to sync database schema")?;

    spin.finish_success("Schema sync complete");

    let migrations_dir = config_dir.join("migrations");
    let pending = db_migrate::get_pending_migrations(pool, &migrations_dir)?;

    if pending.is_empty() {
        cli::info("No pending migrations.");
        return Ok(());
    }

    let hook_runner = HookRunner::builder()
        .config_dir(config_dir)
        .registry(registry.clone())
        .config(cfg)
        .build()?;

    run_migrations(pool, &hook_runner, &migrations_dir, &pending, "up")?;

    cli::success(&format!("{} migration(s) applied.", pending.len()));

    Ok(())
}

/// Rollback the last N applied Lua data migrations.
#[cfg(not(tarpaulin_include))]
fn migrate_down(
    config_dir: &Path,
    cfg: &CrapConfig,
    registry: &Arc<RwLock<Registry>>,
    pool: &DbPool,
    steps: usize,
) -> Result<()> {
    let applied = db_migrate::get_applied_migrations_desc(pool)?;
    let to_rollback: Vec<_> = applied.into_iter().take(steps).collect();

    if to_rollback.is_empty() {
        cli::info("No migrations to roll back.");

        return Ok(());
    }

    let hook_runner = HookRunner::builder()
        .config_dir(config_dir)
        .registry(registry.clone())
        .config(cfg)
        .build()?;

    let migrations_dir = config_dir.join("migrations");

    for filename in &to_rollback {
        let path = migrations_dir.join(filename);

        if !path.exists() {
            bail!("Migration file not found: {}", path.display());
        }

        let mut conn = pool.get().context("Failed to get DB connection")?;
        let tx = conn.transaction().context("Failed to begin transaction")?;

        hook_runner.run_migration(&path, "down", &tx)?;
        db_migrate::remove_migration(&tx, filename)?;

        tx.commit()
            .with_context(|| format!("Failed to commit rollback of {}", filename))?;

        cli::success(&format!("Rolled back: {}", filename));
    }

    cli::success(&format!("{} migration(s) rolled back.", to_rollback.len()));

    Ok(())
}

/// Display migration files with their applied/pending status.
#[cfg(not(tarpaulin_include))]
fn migrate_list(config_dir: &Path, pool: &DbPool) -> Result<()> {
    let migrations_dir = config_dir.join("migrations");
    let all_files = db_migrate::list_migration_files(&migrations_dir)?;
    let applied = db_migrate::get_applied_migrations(pool)?;

    if all_files.is_empty() {
        cli::info(&format!(
            "No migration files found in {}",
            migrations_dir.display()
        ));
        return Ok(());
    }

    let mut table = Table::new(vec!["Migration", "Status"]);

    for f in &all_files {
        let status = if applied.contains(f) {
            "applied"
        } else {
            "pending"
        };

        table.row(vec![f, status]);
    }

    table.print();

    Ok(())
}

/// Drop all tables, recreate schema from Lua definitions, and run all migrations.
#[cfg(not(tarpaulin_include))]
fn migrate_fresh(
    config_dir: &Path,
    cfg: &CrapConfig,
    registry: &Arc<RwLock<Registry>>,
    pool: &DbPool,
    confirm: bool,
) -> Result<()> {
    if !confirm {
        bail!(
            "migrate fresh is destructive — it drops ALL tables and recreates them.\n\
             Pass --confirm to proceed."
        );
    }

    let spin = Spinner::new("Dropping all tables...");
    db_migrate::drop_all_tables(pool)?;
    spin.finish_success("Tables dropped");

    let spin = Spinner::new("Recreating schema...");
    db_migrate::sync_all(pool, registry, &cfg.locale).context("Failed to sync database schema")?;
    spin.finish_success("Schema sync complete");

    let migrations_dir = config_dir.join("migrations");
    let all_files = db_migrate::list_migration_files(&migrations_dir)?;

    if !all_files.is_empty() {
        let hook_runner = HookRunner::builder()
            .config_dir(config_dir)
            .registry(registry.clone())
            .config(cfg)
            .build()?;

        run_migrations(pool, &hook_runner, &migrations_dir, &all_files, "up")?;

        cli::success(&format!("{} migration(s) applied.", all_files.len()));
    }

    cli::success("Fresh migration complete.");

    Ok(())
}

/// Run a list of migration files in order, recording each in the migrations table.
#[cfg(not(tarpaulin_include))]
fn run_migrations(
    pool: &DbPool,
    hook_runner: &HookRunner,
    migrations_dir: &Path,
    filenames: &[String],
    direction: &str,
) -> Result<()> {
    for filename in filenames {
        let path = migrations_dir.join(filename);
        let mut conn = pool.get().context("Failed to get DB connection")?;
        let tx = conn.transaction().context("Failed to begin transaction")?;

        hook_runner.run_migration(&path, direction, &tx)?;
        db_migrate::record_migration(&tx, filename)?;

        tx.commit()
            .with_context(|| format!("Failed to commit migration {}", filename))?;

        cli::success(&format!("Applied: {}", filename));
    }

    Ok(())
}
