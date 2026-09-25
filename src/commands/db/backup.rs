//! `backup` subcommand: database snapshot + optional uploads archive.
//!
//! Everything a backup writes is owner-only (directory `0700`, files `0600`):
//! the snapshot holds password and API-key hashes and sealed TOTP secrets.
//! With `--include-uploads` the uploads tree is captured around the database
//! snapshot so the archive holds every file the snapshot references, even
//! while `serve` keeps writing — see [`super::uploads_snapshot`].

use std::{
    fs, io,
    path::{Path, PathBuf},
    process,
};

use anyhow::{Context as _, Result, bail};
use chrono::Local;

use crate::{
    cli::{self, Spinner},
    commands::{
        db::{
            helpers::{classify_tar_status, create_private_dir, restrict_to_owner},
            manifest::{BACKUP_FORMAT_VERSION, BackupManifest},
            secret::backup_secret,
            uploads_snapshot::{UPLOADS_DIR, UploadsSnapshot},
        },
        helpers::{hold_instance_lock, load_config_for_recovery},
    },
    config::{CrapConfig, DatabaseBackend, UploadStorage, write_new_owner_only},
    core::Builder,
    db::{DbConnection, pool},
};

/// Options of the `backup` subcommand.
#[derive(Builder)]
pub struct BackupOpts {
    /// Output directory (default: `<config_dir>/backups`).
    pub output: Option<PathBuf>,
    /// Also archive the uploads directory.
    pub include_uploads: bool,
    /// Run on a config that fails validation (recovery after an upgrade).
    pub skip_config_validation: bool,
}

/// Handle the `backup` subcommand — create a timestamped database snapshot with optional uploads.
///
/// # Errors
///
/// Returns an error if config loading, pool creation, the DB backup
/// operation, or upload archiving fails.
#[cfg(not(tarpaulin_include))]
pub fn backup(config_dir: &Path, opts: BackupOpts) -> Result<()> {
    let BackupOpts {
        output,
        include_uploads,
        skip_config_validation,
    } = opts;

    let config_dir = config_dir
        .canonicalize()
        .unwrap_or_else(|_| config_dir.to_path_buf());

    let cfg = load_config_for_recovery(&config_dir, skip_config_validation)?;
    ensure_file_database(&cfg)?;
    let _instance_lock = hold_instance_lock(&config_dir)?;
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

    // First capture pass BEFORE the database snapshot: pins every file the
    // snapshot can reference.
    let uploads = include_uploads
        .then(|| capture_local_uploads(&cfg, &config_dir))
        .transpose()?
        .flatten();

    let db_size = backup_database(&config_dir, &cfg, &backup_dir)?;

    let includes_secret = backup_secret(&config_dir, &backup_dir)?;
    if includes_secret {
        cli::info(
            "Included the generated auth secret — keep this backup as private as the secret.",
        );
    }

    let uploads_size = uploads
        .map(|snapshot| archive_uploads(&snapshot, &backup_dir))
        .transpose()?
        .flatten();

    write_backup_manifest(&WriteManifestParams {
        backup_dir: &backup_dir,
        db_path: &db_path,
        config_dir: &config_dir,
        db_size,
        uploads_size,
        include_uploads,
        includes_secret,
    })?;

    cli::success(&format!("Backup complete: {}", backup_dir.display()));

    Ok(())
}

/// Start capturing the uploads when they live in this project (first pass).
/// Uploads kept in another storage are backed up with that service, so they
/// are skipped with a note; so is a project without an uploads directory.
fn capture_local_uploads(cfg: &CrapConfig, config_dir: &Path) -> Result<Option<UploadsSnapshot>> {
    let storage = cfg.upload.storage;

    if !matches!(storage, UploadStorage::Local) {
        cli::info(&format!(
            "Uploads are kept in {storage:?} storage, not in this project — back them up \
             with that service. Skipping."
        ));

        return Ok(None);
    }

    let Some(snapshot) = UploadsSnapshot::create(config_dir).context(UPLOADS_FAILED)? else {
        cli::info("No uploads directory found — skipping.");

        return Ok(None);
    };

    snapshot.capture().context(UPLOADS_FAILED)?;

    Ok(Some(snapshot))
}

/// `backup` copies the `SQLite` database file; a Postgres database is backed up
/// with its own tooling.
fn ensure_file_database(cfg: &CrapConfig) -> Result<()> {
    if cfg.database.backend != DatabaseBackend::Sqlite {
        bail!(
            "`backup` copies the SQLite database file — back up a Postgres database with pg_dump"
        );
    }

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
fn create_backup_dir(config_dir: &Path, output: Option<PathBuf>) -> Result<PathBuf> {
    let timestamp = Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
    let backup_base = output.unwrap_or_else(|| config_dir.join("backups"));
    let backup_dir = backup_base.join(format!("backup-{timestamp}"));

    create_private_dir(&backup_dir).with_context(|| {
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
    restrict_to_owner(&backup_db_path, 0o600)?;

    let db_size = fs::metadata(&backup_db_path).map_or(0, |m| m.len());

    spin.finish_success(&format!(
        "Database snapshot: {} ({} bytes)",
        backup_db_path.display(),
        db_size
    ));

    Ok(db_size)
}

/// What a failed uploads step means for the backup as a whole. `backup` must
/// not go on to report success once the operator asked for uploads and they
/// are not in the backup.
const UPLOADS_FAILED: &str = "Uploads backup failed — no uploads archive was written";

/// Compress the captured `uploads/` under `snapshot_root` into `staged_path`.
///
/// The archive is staged under a temp name — an interrupted `tar` must never
/// leave a truncated `uploads.tar.gz` that looks like a valid backup. The
/// caller renames it into place once `tar` reports success. `tar` reads the
/// private capture, never the live tree, so a concurrent upload cannot make
/// it report a changed file.
#[cfg(not(tarpaulin_include))]
fn run_uploads_tar(snapshot_root: &Path, staged_path: &Path) -> io::Result<process::ExitStatus> {
    process::Command::new("tar")
        .args([
            "czf",
            &staged_path.to_string_lossy(),
            "-C",
            &snapshot_root.to_string_lossy(),
            UPLOADS_DIR,
        ])
        .status()
}

/// Read the `tar` outcome, dropping whatever a failed run staged. `Err` fails
/// the whole `backup` — the same treatment `restore` gives a failed `tar`.
fn finish_uploads_tar(status: io::Result<process::ExitStatus>, staged_path: &Path) -> Result<()> {
    let Err(e) = classify_tar_status(status) else {
        return Ok(());
    };

    let _ = fs::remove_file(staged_path);

    Err(e).context(UPLOADS_FAILED)
}

/// Publish the staged archive under its final name and report its size.
///
/// A rename that fails leaves no archive, so it fails the backup like a failed
/// `tar` does. A size that can't be read does not: the archive itself is in
/// place, only the manifest's recorded size is missing.
fn finalize_uploads_archive(
    spin: &Spinner,
    staged_path: &Path,
    archive_path: &Path,
) -> Result<Option<u64>> {
    let published = restrict_to_owner(staged_path, 0o600)
        .and_then(|()| fs::rename(staged_path, archive_path).map_err(Into::into));

    if let Err(e) = published {
        spin.finish_warning(&format!("Failed to finalize uploads archive: {e:#}"));

        return Err(e).context(UPLOADS_FAILED);
    }

    let size = match fs::metadata(archive_path).map(|m| m.len()) {
        Ok(size) => size,
        Err(e) => {
            spin.finish_warning(&format!(
                "Uploads archive written but its size could not be read: {e}"
            ));

            return Ok(None);
        }
    };

    spin.finish_success(&format!(
        "Uploads archive: {} ({size} bytes)",
        archive_path.display()
    ));

    Ok(Some(size))
}

/// Finish the uploads capture (second pass, after the database snapshot) and
/// compress it into `uploads.tar.gz`. A capture, `tar` or rename failure is an
/// error.
fn archive_uploads(snapshot: &UploadsSnapshot, backup_dir: &Path) -> Result<Option<u64>> {
    let spin = Spinner::new("Compressing uploads...");

    if let Err(e) = snapshot.capture().context(UPLOADS_FAILED) {
        spin.finish_warning(&format!("{e:#}"));

        return Err(e);
    }

    let staged_path = backup_dir.join("uploads.tar.gz.tmp");
    let status = run_uploads_tar(snapshot.root(), &staged_path);

    if let Err(e) = finish_uploads_tar(status, &staged_path) {
        spin.finish_warning(&format!("{e:#}"));

        return Err(e);
    }

    finalize_uploads_archive(&spin, &staged_path, &backup_dir.join("uploads.tar.gz"))
}

/// Args for [`write_backup_manifest`]. Path triple + sizes + what the
/// backup includes — declarative call site instead of positional args at
/// the single dispatch site.
struct WriteManifestParams<'a> {
    backup_dir: &'a Path,
    db_path: &'a Path,
    config_dir: &'a Path,
    db_size: u64,
    uploads_size: Option<u64>,
    include_uploads: bool,
    includes_secret: bool,
}

/// Write the backup manifest.json with metadata about the backup.
#[cfg(not(tarpaulin_include))]
fn write_backup_manifest(p: &WriteManifestParams<'_>) -> Result<()> {
    let manifest = BackupManifest {
        format_version: BACKUP_FORMAT_VERSION,
        crap_version: env!("CARGO_PKG_VERSION").to_string(),
        timestamp: Local::now().to_rfc3339(),
        db_size: p.db_size,
        uploads_size: p.uploads_size,
        include_uploads: p.include_uploads,
        source_db: p.db_path.to_string_lossy().into_owned(),
        source_config: p.config_dir.to_string_lossy().into_owned(),
        includes_secret: p.includes_secret,
    };

    write_new_owner_only(
        &p.backup_dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)?.as_bytes(),
    )
    .context("Failed to write manifest.json")
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::{fs::PermissionsExt as _, process::ExitStatusExt as _};
    #[cfg(unix)]
    use std::{
        io::{Error, ErrorKind},
        process::ExitStatus,
    };

    use super::{
        capture_local_uploads, create_backup_dir, ensure_file_database, finish_uploads_tar,
        preflight_writable,
    };
    use crate::config::{CrapConfig, DatabaseBackend, UploadStorage};

    /// Regression: a `tar` that never ran (binary missing) or exited non-zero
    /// was reported as a warning and the command went on to print "Backup
    /// complete" — the operator asked for uploads and got a backup without
    /// them. Both outcomes now fail the command, and neither leaves the
    /// half-written staging file behind.
    #[cfg(unix)]
    #[test]
    fn a_failed_uploads_tar_fails_the_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = tmp.path().join("uploads.tar.gz.tmp");

        fs::write(&staged, b"truncated").unwrap();
        let missing =
            finish_uploads_tar(Err(Error::from(ErrorKind::NotFound)), &staged).unwrap_err();
        let msg = format!("{missing:#}");
        assert!(msg.contains("no uploads archive was written"), "{msg}");
        assert!(msg.contains("tar not found"), "{msg}");
        assert!(
            !staged.exists(),
            "a failed tar must not leave a staged file"
        );

        // Exit code 1 → wait-status 256 on unix.
        fs::write(&staged, b"truncated").unwrap();
        let nonzero = finish_uploads_tar(Ok(ExitStatus::from_raw(256)), &staged).unwrap_err();
        let msg = format!("{nonzero:#}");
        assert!(msg.contains("no uploads archive was written"), "{msg}");
        assert!(msg.contains("tar exited with status"), "{msg}");
        assert!(!staged.exists());
    }

    /// A `tar` that succeeded leaves the staged archive for the caller to
    /// rename into place.
    #[cfg(unix)]
    #[test]
    fn a_successful_uploads_tar_keeps_the_staged_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let staged = tmp.path().join("uploads.tar.gz.tmp");
        fs::write(&staged, b"archive").unwrap();

        finish_uploads_tar(Ok(ExitStatus::from_raw(0)), &staged).unwrap();

        assert!(staged.exists());
    }

    /// Uploads kept outside the project aren't archived, even when asked for.
    #[test]
    fn backup_skips_uploads_kept_in_another_storage() {
        let config_dir = tempfile::tempdir().unwrap();
        let uploads = config_dir.path().join("uploads").join("media");
        fs::create_dir_all(&uploads).unwrap();
        fs::write(uploads.join("stale-local-copy.png"), b"x").unwrap();

        let mut cfg = CrapConfig::default();
        cfg.upload.storage = UploadStorage::S3;

        assert!(
            capture_local_uploads(&cfg, config_dir.path())
                .unwrap()
                .is_none()
        );
        assert!(
            !config_dir.path().join("data").exists(),
            "no capture directory is created for uploads in another storage"
        );
    }

    /// Local uploads are captured by the first pass, before the database
    /// snapshot runs.
    #[test]
    fn local_uploads_are_captured_before_the_database_snapshot() {
        let config_dir = tempfile::tempdir().unwrap();
        let uploads = config_dir.path().join("uploads").join("media");
        fs::create_dir_all(&uploads).unwrap();
        fs::write(uploads.join("a.png"), b"x").unwrap();

        let snapshot = capture_local_uploads(&CrapConfig::default(), config_dir.path())
            .unwrap()
            .unwrap();
        fs::remove_file(uploads.join("a.png")).unwrap();

        assert!(snapshot.root().join("uploads/media/a.png").is_file());
    }

    /// Regression: the backup directory was created with the umask (typically
    /// world-readable) although it holds password hashes and sealed secrets.
    #[cfg(unix)]
    #[test]
    fn backup_dir_is_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("shared-out");
        fs::create_dir(&out).unwrap();
        fs::set_permissions(&out, fs::Permissions::from_mode(0o777)).unwrap();

        let dir = create_backup_dir(tmp.path(), Some(out)).unwrap();

        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn backup_explains_that_a_postgres_database_needs_pg_dump() {
        let mut cfg = CrapConfig::default();
        assert!(ensure_file_database(&cfg).is_ok());

        cfg.database.backend = DatabaseBackend::Postgres;
        let err = ensure_file_database(&cfg).unwrap_err().to_string();
        assert!(err.contains("pg_dump"), "{err}");
    }

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
