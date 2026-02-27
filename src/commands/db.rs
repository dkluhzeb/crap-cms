//! `migrate`, `db console`, and `backup` commands.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Handle the `migrate` subcommand.
pub fn migrate(config_dir: &Path, action: super::MigrateAction) -> Result<()> {
    let config_dir = config_dir.canonicalize().unwrap_or_else(|_| config_dir.to_path_buf());

    // Create only writes a file — no Lua/DB needed
    if let super::MigrateAction::Create { ref name } = action {
        return crate::scaffold::make_migration(&config_dir, name);
    }

    let cfg = crate::config::CrapConfig::load(&config_dir)
        .context("Failed to load config")?;
    let registry = crate::hooks::init_lua(&config_dir, &cfg)
        .context("Failed to initialize Lua VM")?;
    let pool = crate::db::pool::create_pool(&config_dir, &cfg)
        .context("Failed to create database pool")?;

    match action {
        super::MigrateAction::Create { .. } => unreachable!(),
        super::MigrateAction::Up => {
            // Schema sync from Lua definitions
            println!("Syncing schema from Lua definitions...");
            crate::db::migrate::sync_all(&pool, &registry, &cfg.locale)
                .context("Failed to sync database schema")?;
            println!("Schema sync complete.");

            // Run pending Lua data migrations
            let migrations_dir = config_dir.join("migrations");
            let pending = crate::db::migrate::get_pending_migrations(&pool, &migrations_dir)?;

            if pending.is_empty() {
                println!("No pending migrations.");
            } else {
                let hook_runner = crate::hooks::lifecycle::HookRunner::new(&config_dir, registry, &cfg)?;
                for filename in &pending {
                    let path = migrations_dir.join(filename);
                    let mut conn = pool.get().context("Failed to get DB connection")?;
                    let tx = conn.transaction().context("Failed to begin transaction")?;
                    hook_runner.run_migration(&path, "up", &tx)?;
                    crate::db::migrate::record_migration(&tx, filename)?;
                    tx.commit()
                        .with_context(|| format!("Failed to commit migration {}", filename))?;
                    println!("Applied: {}", filename);
                }
                println!("{} migration(s) applied.", pending.len());
            }
        }
        super::MigrateAction::Down { steps } => {
            let applied = crate::db::migrate::get_applied_migrations_desc(&pool)?;
            let to_rollback: Vec<_> = applied.into_iter().take(steps).collect();

            if to_rollback.is_empty() {
                println!("No migrations to roll back.");
            } else {
                let hook_runner = crate::hooks::lifecycle::HookRunner::new(&config_dir, registry, &cfg)?;
                let migrations_dir = config_dir.join("migrations");
                for filename in &to_rollback {
                    let path = migrations_dir.join(filename);
                    if !path.exists() {
                        anyhow::bail!("Migration file not found: {}", path.display());
                    }
                    let mut conn = pool.get().context("Failed to get DB connection")?;
                    let tx = conn.transaction().context("Failed to begin transaction")?;
                    hook_runner.run_migration(&path, "down", &tx)?;
                    crate::db::migrate::remove_migration(&tx, filename)?;
                    tx.commit()
                        .with_context(|| format!("Failed to commit rollback of {}", filename))?;
                    println!("Rolled back: {}", filename);
                }
                println!("{} migration(s) rolled back.", to_rollback.len());
            }
        }
        super::MigrateAction::List => {
            let migrations_dir = config_dir.join("migrations");
            let all_files = crate::db::migrate::list_migration_files(&migrations_dir)?;
            let applied = crate::db::migrate::get_applied_migrations(&pool)?;

            if all_files.is_empty() {
                println!("No migration files found in {}", migrations_dir.display());
            } else {
                println!("{:<50} Status", "Migration");
                println!("{}", "-".repeat(60));
                for f in &all_files {
                    let status = if applied.contains(f) { "applied" } else { "pending" };
                    println!("{:<50} {}", f, status);
                }
            }
        }
        super::MigrateAction::Fresh { confirm } => {
            if !confirm {
                anyhow::bail!(
                    "migrate fresh is destructive — it drops ALL tables and recreates them.\n\
                     Pass --confirm to proceed."
                );
            }

            println!("Dropping all tables...");
            crate::db::migrate::drop_all_tables(&pool)?;
            println!("Tables dropped.");

            println!("Recreating schema from Lua definitions...");
            crate::db::migrate::sync_all(&pool, &registry, &cfg.locale)
                .context("Failed to sync database schema")?;
            println!("Schema sync complete.");

            // Run all migrations from scratch
            let migrations_dir = config_dir.join("migrations");
            let all_files = crate::db::migrate::list_migration_files(&migrations_dir)?;
            if !all_files.is_empty() {
                let hook_runner = crate::hooks::lifecycle::HookRunner::new(&config_dir, registry, &cfg)?;
                for filename in &all_files {
                    let path = migrations_dir.join(filename);
                    let mut conn = pool.get().context("Failed to get DB connection")?;
                    let tx = conn.transaction().context("Failed to begin transaction")?;
                    hook_runner.run_migration(&path, "up", &tx)?;
                    crate::db::migrate::record_migration(&tx, filename)?;
                    tx.commit()
                        .with_context(|| format!("Failed to commit migration {}", filename))?;
                    println!("Applied: {}", filename);
                }
                println!("{} migration(s) applied.", all_files.len());
            }

            println!("Fresh migration complete.");
        }
    }

    Ok(())
}

/// Open an interactive SQLite console.
pub fn console(config_dir: &Path) -> Result<()> {
    let config_dir = config_dir.canonicalize().unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = crate::config::CrapConfig::load(&config_dir)
        .context("Failed to load config")?;

    let db_path = cfg.db_path(&config_dir);
    if !db_path.exists() {
        anyhow::bail!("Database file not found: {}", db_path.display());
    }

    println!("Opening SQLite console: {}", db_path.display());

    let status = std::process::Command::new("sqlite3")
        .arg(&db_path)
        .status()
        .context("Failed to launch sqlite3 — is it installed?")?;

    if !status.success() {
        anyhow::bail!("sqlite3 exited with status {}", status);
    }

    Ok(())
}

/// Handle the `backup` subcommand.
pub fn backup(
    config_dir: &Path,
    output: Option<PathBuf>,
    include_uploads: bool,
) -> Result<()> {
    let config_dir = config_dir.canonicalize().unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = crate::config::CrapConfig::load(&config_dir)
        .context("Failed to load config")?;

    let db_path = cfg.db_path(&config_dir);
    if !db_path.exists() {
        anyhow::bail!("Database file not found: {}", db_path.display());
    }

    // Determine backup directory
    let timestamp = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let backup_dir_name = format!("backup-{}", timestamp);
    let backup_base = output.unwrap_or_else(|| config_dir.join("backups"));
    let backup_dir = backup_base.join(&backup_dir_name);

    std::fs::create_dir_all(&backup_dir)
        .with_context(|| format!("Failed to create backup directory: {}", backup_dir.display()))?;

    // VACUUM INTO for a consistent snapshot
    let backup_db_path = backup_dir.join("crap.db");
    println!("Creating database snapshot...");
    {
        let conn = rusqlite::Connection::open(&db_path)
            .context("Failed to open database for backup")?;
        conn.execute("VACUUM INTO ?1", [backup_db_path.to_string_lossy().as_ref()])
            .context("VACUUM INTO failed")?;
    }
    let db_size = std::fs::metadata(&backup_db_path)
        .map(|m| m.len())
        .unwrap_or(0);
    println!("Database snapshot: {} ({} bytes)", backup_db_path.display(), db_size);

    // Optionally backup uploads
    let mut uploads_size: Option<u64> = None;
    if include_uploads {
        let uploads_dir = config_dir.join("uploads");
        if uploads_dir.exists() && uploads_dir.is_dir() {
            let archive_path = backup_dir.join("uploads.tar.gz");
            println!("Compressing uploads...");
            let status = std::process::Command::new("tar")
                .args(["czf", &archive_path.to_string_lossy(), "-C", &config_dir.to_string_lossy(), "uploads"])
                .status();
            match status {
                Ok(s) if s.success() => {
                    uploads_size = std::fs::metadata(&archive_path).map(|m| m.len()).ok();
                    println!("Uploads archive: {} ({} bytes)",
                        archive_path.display(),
                        uploads_size.unwrap_or(0));
                }
                Ok(s) => {
                    eprintln!("Warning: tar exited with status {}", s);
                }
                Err(e) => {
                    eprintln!("Warning: tar not found or failed: {}. Skipping uploads backup.", e);
                }
            }
        } else {
            println!("No uploads directory found — skipping.");
        }
    }

    // Write manifest.json
    let manifest = serde_json::json!({
        "timestamp": chrono::Local::now().to_rfc3339(),
        "db_size": db_size,
        "uploads_size": uploads_size,
        "include_uploads": include_uploads,
        "source_db": db_path.to_string_lossy(),
        "source_config": config_dir.to_string_lossy(),
    });
    let manifest_path = backup_dir.join("manifest.json");
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)
        .context("Failed to write manifest.json")?;

    println!("\nBackup complete: {}", backup_dir.display());
    Ok(())
}
