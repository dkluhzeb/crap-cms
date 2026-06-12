//! `restore` subcommand: replace database and uploads from a backup.

use std::{fs, path::Path, process};

use anyhow::{Context as _, Result, bail};

use crate::{
    cli::{self, Spinner},
    commands::db::manifest::BackupManifest,
    config::CrapConfig,
    db::{DbConnection, pool},
};

/// Handle the `restore` subcommand — replace database and optionally uploads from a backup.
///
/// # Errors
///
/// Returns an error if the backup directory is invalid, config loading
/// fails, or any of the restore filesystem operations fails.
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

    let manifest: BackupManifest =
        serde_json::from_str(&manifest_str).context("Failed to parse manifest.json")?;

    cli::header("Restoring from backup");

    cli::kv("Version", &manifest.crap_version);
    cli::kv("Timestamp", &manifest.timestamp);
    cli::kv("DB size", &format!("{} bytes", manifest.db_size));

    if let Some(size) = manifest.uploads_size {
        cli::kv("Uploads", &format!("{size} bytes"));
    }

    if manifest.crap_version != env!("CARGO_PKG_VERSION") {
        cli::warning(&format!(
            "Backup was taken with crap-cms {} but this binary is {} — the restored \
             database will be schema-migrated on the next start.",
            manifest.crap_version,
            env!("CARGO_PKG_VERSION")
        ));
    }

    Ok(())
}

/// Replace the live database with the backup copy — crash-safe at every step.
///
/// Sequence:
/// 1. checkpoint the current DB so its WAL folds into the main file (a stale
///    WAL applied to the restored file would corrupt it; folding it first
///    also keeps the kept-aside copy self-consistent),
/// 2. stage the backup copy next to the target (same dir → atomic rename),
/// 3. delete the old sidecars (hard error — never proceed past a leftover),
/// 4. move the old DB aside as `*.pre-restore` and rename the staged copy
///    into place. An interrupt at any point leaves either the old or the
///    new database intact on disk, never a half-written one.
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

    let sidecars = checkpoint_and_list_sidecars(config_dir, cfg, db_path)?;

    let spin = Spinner::new("Restoring database...");

    let staged = db_path.with_extension("db.restore-tmp");
    fs::copy(backup_dir.join("crap.db"), &staged)
        .with_context(|| format!("Failed to stage database copy at {}", staged.display()))?;

    for sidecar in &sidecars {
        if sidecar.exists() {
            fs::remove_file(sidecar).with_context(|| {
                format!(
                    "Failed to remove {} — aborting before touching the database",
                    sidecar.display()
                )
            })?;
        }
    }

    let aside = db_path.with_extension("db.pre-restore");
    if db_path.exists() {
        fs::rename(db_path, &aside).with_context(|| {
            format!(
                "Failed to move the previous database to {}",
                aside.display()
            )
        })?;
    }

    fs::rename(&staged, db_path)
        .with_context(|| format!("Failed to move restored database to {}", db_path.display()))?;

    spin.finish_success("Database restored");

    if aside.exists() {
        cli::info(&format!(
            "Previous database kept at {} — delete it once the restore is verified.",
            aside.display()
        ));
    }

    Ok(())
}

/// Open the *current* database, fold its WAL into the main file, and return
/// the sidecar paths to clean up. Refuses non-SQLite backends — the backup
/// format is a `SQLite` file.
#[cfg(not(tarpaulin_include))]
fn checkpoint_and_list_sidecars(
    config_dir: &Path,
    cfg: &CrapConfig,
    db_path: &Path,
) -> Result<Vec<std::path::PathBuf>> {
    let pool = pool::create_pool(config_dir, cfg).context("Failed to create database pool")?;
    let conn = pool.get().context("Failed to get DB connection")?;

    if conn.kind() != "sqlite" {
        bail!("restore supports the SQLite backend only");
    }

    conn.query_all("PRAGMA wal_checkpoint(TRUNCATE)", &[])
        .context("Failed to checkpoint the current database")?;

    Ok(conn
        .sidecar_extensions()
        .iter()
        .map(|ext| db_path.with_extension(ext))
        .collect())
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
        Ok(s) => spin.finish_warning(&format!("tar exited with status {s}")),
        Err(e) => spin.finish_warning(&format!("tar not found or failed: {e}. Skipping.")),
    }
}
