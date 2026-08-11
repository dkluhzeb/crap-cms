//! `backup` subcommand: database snapshot + optional uploads archive.

use std::{
    fs,
    path::{Path, PathBuf},
    process,
};

use anyhow::{Context as _, Result, bail};
use chrono::Local;

use crate::{
    cli::{self, Spinner},
    commands::db::manifest::BackupManifest,
    config::CrapConfig,
    db::{DbConnection, pool},
};

/// Handle the `backup` subcommand — create a timestamped database snapshot with optional uploads.
///
/// # Errors
///
/// Returns an error if config loading, pool creation, the DB backup
/// operation, or upload archiving fails.
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

    // Pre-flight: confirm the chosen output directory exists (or can be created)
    // and is writable BEFORE we perform a long-running VACUUM INTO snapshot.
    let backup_base = output.clone().unwrap_or_else(|| config_dir.join("backups"));
    preflight_writable(&backup_base).with_context(|| {
        format!(
            "Backup output directory not writable: {}",
            backup_base.display()
        )
    })?;

    let backup_dir = create_backup_dir(&config_dir, output)?;
    let db_size = backup_database(&config_dir, &cfg, &backup_dir)?;

    let uploads_size = if include_uploads {
        backup_uploads(&config_dir, &backup_dir)
    } else {
        None
    };

    write_backup_manifest(&WriteManifestParams {
        backup_dir: &backup_dir,
        db_path: &db_path,
        config_dir: &config_dir,
        db_size,
        uploads_size,
        include_uploads,
    })?;

    cli::success(&format!("Backup complete: {}", backup_dir.display()));

    Ok(())
}

/// Verify the backup output directory exists (or can be created) and accepts
/// new files. Avoids partial backups that would only fail mid-way at manifest
/// write time. We create the directory if missing, then write+delete a probe
/// file via a uniquely named path.
fn preflight_writable(base: &Path) -> Result<()> {
    fs::create_dir_all(base)
        .with_context(|| format!("Failed to create backup base directory: {}", base.display()))?;

    let probe = base.join(format!(".crap-backup-probe-{}", std::process::id()));

    // Create + immediately drop. If the directory is read-only, this errors.
    fs::File::create(&probe)
        .with_context(|| format!("Failed to write probe file in {}", base.display()))?;

    // Best-effort cleanup; ignore failure (dir may be sticky etc).
    let _ = fs::remove_file(&probe);

    Ok(())
}

/// Create the timestamped backup directory.
#[cfg(not(tarpaulin_include))]
fn create_backup_dir(config_dir: &Path, output: Option<PathBuf>) -> Result<PathBuf> {
    let timestamp = Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let backup_base = output.unwrap_or_else(|| config_dir.join("backups"));
    let backup_dir = backup_base.join(format!("backup-{timestamp}"));

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

    let db_size = fs::metadata(&backup_db_path).map_or(0, |m| m.len());

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
    // Write to a temp name and rename on success — an interrupted tar must
    // never leave a truncated uploads.tar.gz that looks like a valid backup.
    let staged_path = backup_dir.join("uploads.tar.gz.tmp");
    let spin = Spinner::new("Compressing uploads...");

    let status = process::Command::new("tar")
        .args([
            "czf",
            &staged_path.to_string_lossy(),
            "-C",
            &config_dir.to_string_lossy(),
            "uploads",
        ])
        .status();

    match status {
        Ok(s) if s.success() => {
            if let Err(e) = fs::rename(&staged_path, &archive_path) {
                spin.finish_warning(&format!("Failed to finalize uploads archive: {e}"));
                return None;
            }

            match fs::metadata(&archive_path).map(|m| m.len()) {
                Ok(size) => {
                    spin.finish_success(&format!(
                        "Uploads archive: {} ({size} bytes)",
                        archive_path.display()
                    ));
                    Some(size)
                }
                Err(e) => {
                    spin.finish_warning(&format!(
                        "Uploads archive written but its size could not be read: {e}"
                    ));
                    None
                }
            }
        }
        Ok(s) => {
            let _ = fs::remove_file(&staged_path);
            spin.finish_warning(&format!("tar exited with status {s}"));
            None
        }
        Err(e) => {
            let _ = fs::remove_file(&staged_path);
            spin.finish_warning(&format!(
                "tar not found or failed: {e}. Skipping uploads backup."
            ));
            None
        }
    }
}

/// Args for [`write_backup_manifest`]. Path triple + sizes +
/// the `--include-uploads` flag — declarative call site instead
/// of 6 positional args at the single dispatch site.
struct WriteManifestParams<'a> {
    backup_dir: &'a Path,
    db_path: &'a Path,
    config_dir: &'a Path,
    db_size: u64,
    uploads_size: Option<u64>,
    include_uploads: bool,
}

/// Write the backup manifest.json with metadata about the backup.
#[cfg(not(tarpaulin_include))]
fn write_backup_manifest(p: &WriteManifestParams<'_>) -> Result<()> {
    let manifest = BackupManifest {
        format_version: crate::commands::db::manifest::BACKUP_FORMAT_VERSION,
        crap_version: env!("CARGO_PKG_VERSION").to_string(),
        timestamp: Local::now().to_rfc3339(),
        db_size: p.db_size,
        uploads_size: p.uploads_size,
        include_uploads: p.include_uploads,
        source_db: p.db_path.to_string_lossy().into_owned(),
        source_config: p.config_dir.to_string_lossy().into_owned(),
    };

    fs::write(
        p.backup_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?,
    )
    .context("Failed to write manifest.json")
}

#[cfg(test)]
mod tests {
    use super::preflight_writable;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn preflight_succeeds_on_new_subdir() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("new-backups");
        preflight_writable(&sub).expect("should create + write");
        assert!(sub.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn backup_errors_early_on_read_only_output_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ro = tmp.path().join("ro-out");
        fs::create_dir(&ro).unwrap();

        // Mode 0o555: readable/executable but NOT writable.
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();

        let err = preflight_writable(&ro).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.to_lowercase().contains("permission")
                || msg.contains("probe")
                || msg.contains("Failed"),
            "expected a clear write-failure message: {msg}"
        );

        // Restore perms for cleanup.
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
