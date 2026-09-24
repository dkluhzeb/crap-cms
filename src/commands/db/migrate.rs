//! `migrate` subcommand: schema sync, Lua data migrations, rollback, fresh.

use std::{path::Path, sync::Arc};

use anyhow::{Context as _, Result, bail};

use crate::{
    cli::{self, Spinner, Table},
    commands::{MigrateAction, helpers, load_config},
    config::CrapConfig,
    core::Registry,
    db::{DbConnection, DbPool, migrate as db_migrate, pool},
    hooks::{self, LuaCrudInfra, MigrationCall},
    scaffold,
    service::AppInfra,
};

/// Handle the `migrate` subcommand — dispatches to the appropriate action handler.
///
/// # Errors
///
/// Returns an error if config loading, pool creation, or the dispatched
/// migration action fails.
#[cfg(not(tarpaulin_include))]
pub fn migrate(config_dir: &Path, action: &MigrateAction) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    if let MigrateAction::Create { name } = action {
        return scaffold::make_migration(&config_dir, name);
    }

    if let MigrateAction::Fresh { confirm: false } = action {
        bail!(
            "migrate fresh is destructive — it drops ALL tables and recreates them.\n\
             Pass --confirm to proceed."
        );
    }

    // Migrations run Lua and write through the database like any command that
    // opens the project, so the config is validated and put into service first.
    let cfg = load_config(&config_dir)?;

    // Held before the Lua VM and the pool open the database, and for the whole
    // command: exclusively for `fresh`, which drops every table, and shared
    // otherwise, so a restore can't replace the database underneath.
    let _instance_lock = match action {
        MigrateAction::Fresh { .. } => {
            helpers::hold_exclusive_instance_lock(&config_dir, "migrate fresh")?
        }
        _ => helpers::hold_instance_lock(&config_dir)?,
    };

    let registry = hooks::init_lua(&config_dir, &cfg).context("Failed to initialize Lua VM")?;
    let pool = pool::create_pool(&config_dir, &cfg).context("Failed to create database pool")?;

    match action {
        MigrateAction::Create { .. } => unreachable!(),
        MigrateAction::Up => migrate_up(&config_dir, &cfg, &registry, &pool),
        MigrateAction::Down { steps } => migrate_down(&config_dir, &cfg, &registry, &pool, *steps),
        MigrateAction::List => migrate_list(&config_dir, &pool),
        MigrateAction::Fresh { .. } => migrate_fresh(&config_dir, &cfg, &registry, &pool),
    }
}

/// Sync schema from Lua definitions and apply pending Lua data migrations.
#[cfg(not(tarpaulin_include))]
fn migrate_up(
    config_dir: &Path,
    cfg: &CrapConfig,
    registry: &Arc<Registry>,
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

    let infra = helpers::cli_infra(config_dir, registry, cfg, pool)?;

    run_migrations(&infra, &migrations_dir, &pending, "up")?;

    cli::success(&format!("{} migration(s) applied.", pending.len()));

    Ok(())
}

/// Rollback the last N applied Lua data migrations.
#[cfg(not(tarpaulin_include))]
fn migrate_down(
    config_dir: &Path,
    cfg: &CrapConfig,
    registry: &Arc<Registry>,
    pool: &DbPool,
    steps: usize,
) -> Result<()> {
    let applied = db_migrate::get_applied_migrations_desc(pool)?;
    let to_rollback: Vec<_> = applied.into_iter().take(steps).collect();

    if to_rollback.is_empty() {
        cli::info("No migrations to roll back.");

        return Ok(());
    }

    let infra = helpers::cli_infra(config_dir, registry, cfg, pool)?;
    let migrations_dir = config_dir.join("migrations");

    for filename in &to_rollback {
        let path = migrations_dir.join(filename);

        if !path.exists() {
            bail!("Migration file not found: {}", path.display());
        }

        run_one(&infra, &MigrationCall::new(&path, "down"), |conn| {
            db_migrate::remove_migration(conn, filename)
        })
        .with_context(|| format!("Failed to roll back {filename}"))?;

        cli::success(&format!("Rolled back: {filename}"));
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

/// Drop every table and recreate the schema from the Lua definitions.
#[cfg(not(tarpaulin_include))]
fn recreate_schema(cfg: &CrapConfig, registry: &Arc<Registry>, pool: &DbPool) -> Result<()> {
    let spin = Spinner::new("Dropping all tables...");
    db_migrate::drop_all_tables(pool)?;
    spin.finish_success("Tables dropped");

    let spin = Spinner::new("Recreating schema...");
    db_migrate::sync_all(pool, registry, &cfg.locale).context("Failed to sync database schema")?;
    spin.finish_success("Schema sync complete");

    Ok(())
}

/// Drop all tables, recreate schema from Lua definitions, and run all
/// migrations. The caller holds the instance lock.
///
/// Everything that can fail without touching the database — listing the
/// migration files and building the infrastructure they run on (a configured
/// Redis is pinged here) — happens before the first table is dropped, so an
/// unreachable Redis fails the command with the database still intact.
#[cfg(not(tarpaulin_include))]
fn migrate_fresh(
    config_dir: &Path,
    cfg: &CrapConfig,
    registry: &Arc<Registry>,
    pool: &DbPool,
) -> Result<()> {
    let migrations_dir = config_dir.join("migrations");
    let all_files = db_migrate::list_migration_files(&migrations_dir)?;

    let infra = if all_files.is_empty() {
        None
    } else {
        Some(helpers::cli_infra(config_dir, registry, cfg, pool)?)
    };

    recreate_schema(cfg, registry, pool)?;

    if let Some(infra) = infra {
        run_migrations(&infra, &migrations_dir, &all_files, "up")?;

        cli::success(&format!("{} migration(s) applied.", all_files.len()));
    }

    cli::success("Fresh migration complete.");

    Ok(())
}

/// Run one migration on the CLI's infrastructure — the configured cache,
/// live transports and storage — so its writes behave like the server's:
/// `record` runs in the migration's transaction, and the cache clear, live
/// events and upload-file removal all follow the commit.
#[cfg(not(tarpaulin_include))]
fn run_one(
    infra: &AppInfra,
    call: &MigrationCall<'_>,
    record: impl FnOnce(&dyn DbConnection) -> Result<()>,
) -> Result<()> {
    infra.hook_runner.run_migration(
        call,
        &infra.pool,
        Some(LuaCrudInfra::for_pool_crud(infra)),
        record,
    )
}

/// Run a list of migration files in order, recording each in the migrations table.
#[cfg(not(tarpaulin_include))]
fn run_migrations(
    infra: &AppInfra,
    migrations_dir: &Path,
    filenames: &[String],
    direction: &str,
) -> Result<()> {
    for filename in filenames {
        let path = migrations_dir.join(filename);

        run_one(infra, &MigrationCall::new(&path, direction), |conn| {
            db_migrate::record_migration(conn, filename)
        })
        .with_context(|| format!("Failed to apply {filename}"))?;

        cli::success(&format!("Applied: {filename}"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::config::LiveTransport;

    /// Regression: `migrate fresh` built the migrations' infrastructure only
    /// after dropping every table, so an unreachable Redis failed the command
    /// with the database already emptied. The infrastructure is now built
    /// first, and a failure leaves every table in place.
    #[test]
    fn fresh_with_unreachable_redis_fails_before_dropping_anything() {
        let dir = TempDir::new().unwrap();
        let migrations_dir = dir.path().join("migrations");
        fs::create_dir_all(&migrations_dir).unwrap();
        fs::write(
            migrations_dir.join("20260101000000_seed.lua"),
            "local M = {}\nfunction M.up() end\nfunction M.down() end\nreturn M\n",
        )
        .unwrap();

        let mut cfg = CrapConfig::test_default();
        cfg.database.path = "test.db".into();
        cfg.live.enabled = true;
        cfg.live.transport = LiveTransport::Redis;
        cfg.cache.redis_url = "redis://127.0.0.1:1/".into();

        let db_pool = pool::create_pool(dir.path(), &cfg).unwrap();
        db_pool
            .get()
            .unwrap()
            .execute_batch("CREATE TABLE keep_me (id TEXT)")
            .unwrap();

        let registry = Arc::new(Registry::default());

        let result = migrate_fresh(dir.path(), &cfg, &registry, &db_pool);

        assert!(
            result.is_err(),
            "an unreachable Redis must fail the command"
        );
        assert!(
            db_pool.get().unwrap().table_exists("keep_me").unwrap(),
            "no table may be dropped before the infrastructure is built"
        );
    }
}
