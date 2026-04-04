//! `restore` subcommand: replace database and uploads from a backup.

use std::{fs, path::Path, process};

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

use crate::{
    cli::{self, Spinner},
    config::CrapConfig,
    db::{DbConnection, pool},
};

/// Handle the `restore` subcommand — replace database and optionally uploads from a backup.
#[cfg(not(tarpaulin_include))]
pub fn restore(
    config_dir: &Path,
    backup_dir: &Path,
    include_uploads: bool,
    confirm: bool,
) -> Result<()> {
    if !confirm {
        bail!(
            "Restore is destructive — it replaces the current database.\n\
             Pass --confirm / -y to proceed."
        );
    }

    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());
    let backup_dir = backup_dir
        .canonicalize()
        .unwrap_or_else(|_| backup_dir.to_path_buf());

    validate_backup_dir(&backup_dir)?;
    read_and_display_manifest(&backup_dir)?;

    let cfg = CrapConfig::load(&config_dir).context("Failed to load config")?;
    let db_path = cfg.db_path(&config_dir);

    restore_database(&config_dir, &cfg, &backup_dir, &db_path)?;

    if include_uploads {
        restore_uploads(&config_dir, &backup_dir);
    }

    cli::success("Restore complete.");

    Ok(())
}

/// Validate that the backup directory contains required files.
#[cfg(not(tarpaulin_include))]
fn validate_backup_dir(backup_dir: &Path) -> Result<()> {
    if !backup_dir.join("manifest.json").exists() {
        bail!("No manifest.json found in {}", backup_dir.display());
    }

    if !backup_dir.join("crap.db").exists() {
        bail!("No crap.db found in {}", backup_dir.display());
    }

    Ok(())
}

/// Read and display the backup manifest to the user.
#[cfg(not(tarpaulin_include))]
fn read_and_display_manifest(backup_dir: &Path) -> Result<()> {
    let manifest_str = fs::read_to_string(backup_dir.join("manifest.json"))
        .context("Failed to read manifest.json")?;

    let manifest: Value =
        serde_json::from_str(&manifest_str).context("Failed to parse manifest.json")?;

    cli::header("Restoring from backup");

    cli::kv(
        "Version",
        manifest
            .get("crap_version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown"),
    );

    cli::kv(
        "Timestamp",
        manifest
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown"),
    );

    cli::kv(
        "DB size",
        &format!(
            "{} bytes",
            manifest
                .get("db_size")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        ),
    );

    if let Some(size) = manifest.get("uploads_size").and_then(|v| v.as_u64()) {
        cli::kv("Uploads", &format!("{} bytes", size));
    }

    Ok(())
}

/// Copy the backup database to the target path and clean up sidecar files.
#[cfg(not(tarpaulin_include))]
fn restore_database(
    config_dir: &Path,
    cfg: &CrapConfig,
    backup_dir: &Path,
    db_path: &Path,
) -> Result<()> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    let spin = Spinner::new("Restoring database...");

    fs::copy(backup_dir.join("crap.db"), db_path)
        .with_context(|| format!("Failed to copy database to {}", db_path.display()))?;

    spin.finish_success("Database restored");

    // Remove sidecar files (WAL/SHM) left from the previous database
    let pool = pool::create_pool(config_dir, cfg).context("Failed to create database pool")?;
    let conn = pool.get().context("Failed to get DB connection")?;

    for ext in conn.sidecar_extensions() {
        let sidecar = db_path.with_extension(ext);

        if sidecar.exists() {
            let _ = fs::remove_file(&sidecar);
        }
    }

    Ok(())
}

/// Extract the uploads.tar.gz archive from the backup directory.
#[cfg(not(tarpaulin_include))]
fn restore_uploads(config_dir: &Path, backup_dir: &Path) {
    let archive_path = backup_dir.join("uploads.tar.gz");

    if !archive_path.exists() {
        cli::info("No uploads.tar.gz in backup — skipping uploads restore.");
        return;
    }

    let spin = Spinner::new("Extracting uploads...");

    let status = process::Command::new("tar")
        .args([
            "xzf",
            &archive_path.to_string_lossy(),
            "-C",
            &config_dir.to_string_lossy(),
        ])
        .status();

    match status {
        Ok(s) if s.success() => spin.finish_success("Uploads restored"),
        Ok(s) => spin.finish_warning(&format!("tar exited with status {}", s)),
        Err(e) => spin.finish_warning(&format!("tar not found or failed: {}. Skipping.", e)),
    }
}
