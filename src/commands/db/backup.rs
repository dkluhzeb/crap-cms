//! `backup` subcommand: database snapshot + optional uploads archive.

use std::{
    fs,
    path::{Path, PathBuf},
    process,
};

use anyhow::{Context as _, Result, bail};
use serde_json::json;

use crate::{
    cli::{self, Spinner},
    config::CrapConfig,
    db::{DbConnection, pool},
};

/// Handle the `backup` subcommand — create a timestamped database snapshot with optional uploads.
#[cfg(not(tarpaulin_include))]
pub fn backup(config_dir: &Path, output: Option<PathBuf>, include_uploads: bool) -> Result<()> {
    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;
    let db_path = cfg.db_path(&config_dir);

    if !db_path.exists() {
        bail!("Database file not found: {}", db_path.display());
    }

    let backup_dir = create_backup_dir(&config_dir, output)?;
    let db_size = backup_database(&config_dir, &cfg, &backup_dir)?;

    let uploads_size = if include_uploads {
        backup_uploads(&config_dir, &backup_dir)
    } else {
        None
    };

    write_backup_manifest(
        &backup_dir,
        &db_path,
        &config_dir,
        db_size,
        uploads_size,
        include_uploads,
    )?;

    cli::success(&format!("Backup complete: {}", backup_dir.display()));

    Ok(())
}

/// Create the timestamped backup directory.
#[cfg(not(tarpaulin_include))]
fn create_backup_dir(config_dir: &Path, output: Option<PathBuf>) -> Result<PathBuf> {
    let timestamp = chrono::Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let backup_base = output.unwrap_or_else(|| config_dir.join("backups"));
    let backup_dir = backup_base.join(format!("backup-{}", timestamp));

    fs::create_dir_all(&backup_dir).with_context(|| {
        format!(
            "Failed to create backup directory: {}",
            backup_dir.display()
        )
    })?;

    Ok(backup_dir)
}

/// Create a consistent database snapshot using VACUUM INTO.
#[cfg(not(tarpaulin_include))]
fn backup_database(config_dir: &Path, cfg: &CrapConfig, backup_dir: &Path) -> Result<u64> {
    let backup_db_path = backup_dir.join("crap.db");
    let spin = Spinner::new("Creating database snapshot...");

    let pool = pool::create_pool(config_dir, cfg).context("Failed to create database pool")?;
    let conn = pool
        .get()
        .context("Failed to get DB connection for backup")?;

    conn.vacuum_into(&backup_db_path)
        .context("VACUUM INTO failed")?;

    let db_size = fs::metadata(&backup_db_path).map(|m| m.len()).unwrap_or(0);

    spin.finish_success(&format!(
        "Database snapshot: {} ({} bytes)",
        backup_db_path.display(),
        db_size
    ));

    Ok(db_size)
}

/// Compress the uploads directory into a tar.gz archive. Returns the archive size if successful.
#[cfg(not(tarpaulin_include))]
fn backup_uploads(config_dir: &Path, backup_dir: &Path) -> Option<u64> {
    let uploads_dir = config_dir.join("uploads");

    if !uploads_dir.exists() || !uploads_dir.is_dir() {
        cli::info("No uploads directory found — skipping.");
        return None;
    }

    let archive_path = backup_dir.join("uploads.tar.gz");
    let spin = Spinner::new("Compressing uploads...");

    let status = process::Command::new("tar")
        .args([
            "czf",
            &archive_path.to_string_lossy(),
            "-C",
            &config_dir.to_string_lossy(),
            "uploads",
        ])
        .status();

    match status {
        Ok(s) if s.success() => {
            let size = fs::metadata(&archive_path).map(|m| m.len()).ok();

            spin.finish_success(&format!(
                "Uploads archive: {} ({} bytes)",
                archive_path.display(),
                size.unwrap_or(0)
            ));

            size
        }
        Ok(s) => {
            spin.finish_warning(&format!("tar exited with status {}", s));
            None
        }
        Err(e) => {
            spin.finish_warning(&format!(
                "tar not found or failed: {}. Skipping uploads backup.",
                e
            ));
            None
        }
    }
}

/// Write the backup manifest.json with metadata about the backup.
#[cfg(not(tarpaulin_include))]
fn write_backup_manifest(
    backup_dir: &Path,
    db_path: &Path,
    config_dir: &Path,
    db_size: u64,
    uploads_size: Option<u64>,
    include_uploads: bool,
) -> Result<()> {
    let manifest = json!({
        "crap_version": env!("CARGO_PKG_VERSION"),
        "timestamp": chrono::Local::now().to_rfc3339(),
        "db_size": db_size,
        "uploads_size": uploads_size,
        "include_uploads": include_uploads,
        "source_db": db_path.to_string_lossy(),
        "source_config": config_dir.to_string_lossy(),
    });

    fs::write(
        backup_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )
    .context("Failed to write manifest.json")
}
