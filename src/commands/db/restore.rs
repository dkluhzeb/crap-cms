//! `restore` subcommand: replace database and uploads from a backup.

use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process,
};

use anyhow::{Context as _, Result, bail};

use crate::{
    cli::{self, Spinner},
    commands::{
        db::{
            helpers::classify_tar_status,
            manifest::{BACKUP_FORMAT_VERSION, BackupManifest},
            secret::{configured_secret_overrides, has_generated_secret, restore_secret},
        },
        helpers,
    },
    config::{CrapConfig, UploadStorage},
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

    let config_dir = canonical(config_dir);
    let backup_dir = canonical(backup_dir);

    validate_backup_dir(&backup_dir)?;
    read_and_display_manifest(&backup_dir)?;

    let (cfg, had_secret) = load_project(&config_dir)?;
    let db_path = cfg.db_path(&config_dir);

    // Before anything is replaced, and for the whole command.
    let _instance_lock = helpers::hold_exclusive_instance_lock(&config_dir, "restore")?;

    restore_database(&config_dir, &cfg, &backup_dir, &db_path)?;

    if !cfg.auth.secret_generated
        && configured_secret_overrides(&backup_dir, cfg.auth.secret.as_ref())
    {
        cli::warning(
            "`[auth] secret` is set in crap.toml, so the auth secret restored from the backup \
             is not used: its sessions, TOTP secrets and encrypted data stay unreadable until \
             crap.toml sets that secret.",
        );
    }
    restore_secret(&config_dir, &backup_dir, had_secret)?;

    if include_uploads {
        restore_local_uploads(&cfg, &config_dir, &backup_dir)?;
    }

    cli::success("Restore complete.");

    Ok(())
}

/// `path` made absolute, or unchanged when it can't be resolved.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Load the project's config. Loading a directory's config writes into it (a
/// generated auth secret), so a directory that isn't a project is refused
/// first. Also returns whether the project had a generated secret before the
/// load: one the load generates holds nothing the backup's doesn't replace, so
/// it isn't kept aside.
#[cfg(not(tarpaulin_include))]
fn load_project(config_dir: &Path) -> Result<(CrapConfig, bool)> {
    if !config_dir.join("crap.toml").is_file() {
        bail!(
            "{} is not a crap-cms project (no crap.toml)",
            config_dir.display()
        );
    }

    let had_secret = has_generated_secret(config_dir);
    let cfg = CrapConfig::load(config_dir).context("Failed to load config")?;

    Ok((cfg, had_secret))
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

    // Refuse a backup written by a NEWER format than this binary understands —
    // we cannot know how to read a future layout, and guessing risks a corrupt
    // restore. Older/equal (incl. pre-versioning → 1) is accepted.
    if manifest.format_version > BACKUP_FORMAT_VERSION {
        bail!(
            "This backup uses format version {} but this crap-cms only supports up to {}. \
             Upgrade crap-cms to restore it.",
            manifest.format_version,
            BACKUP_FORMAT_VERSION
        );
    }

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

/// Drop the current database's `-wal` / `-shm` sidecars. They belong to the
/// database being replaced; left in place, they would be applied to the
/// restored file and corrupt it. The checkpoint folded the WAL into the main
/// file first, so nothing is lost with them.
///
/// Removal is unconditional and tolerates a missing file: a `wal_checkpoint`
/// or the OS can drop a sidecar between listing and removal, so an `exists()`
/// guard would race. Any other failure is a hard error — never proceed past a
/// leftover.
fn remove_sidecars(sidecars: &[PathBuf]) -> Result<()> {
    for sidecar in sidecars {
        match fs::remove_file(sidecar) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "Failed to remove {} — aborting before touching the database",
                        sidecar.display()
                    )
                });
            }
        }
    }

    Ok(())
}

/// Give the current database a second name, `*.db.pre-restore`, *without*
/// unlinking the live one — so the restore's final rename replaces only the
/// live directory entry and the operator keeps a working copy either way.
///
/// A hard link is instant and shares the bytes (the aside name survives the
/// overwriting rename because it points at the old inode). Filesystems that
/// refuse hard links fall back to a full copy.
fn keep_previous_database(db_path: &Path, aside: &Path) -> Result<()> {
    // A leftover from an earlier restore would make the link fail.
    match fs::remove_file(aside) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| {
                format!("Failed to clear the earlier copy at {}", aside.display())
            });
        }
    }

    if fs::hard_link(db_path, aside).is_ok() {
        return Ok(());
    }

    fs::copy(db_path, aside).with_context(|| {
        format!(
            "Failed to keep the previous database at {}",
            aside.display()
        )
    })?;

    Ok(())
}

/// Replace the live database with the backup copy — crash-safe at every step.
///
/// Sequence:
/// 1. checkpoint the current DB so its WAL folds into the main file (a stale
///    WAL applied to the restored file would corrupt it; folding it first
///    also makes the kept-aside copy self-consistent),
/// 2. stage the backup copy next to the target (same dir → atomic rename),
/// 3. give the current database its second `*.pre-restore` name, leaving the
///    live one in place,
/// 4. delete the old sidecars (hard error — never proceed past a leftover),
/// 5. rename the staged copy over the target.
///
/// The live path is never absent: the only operation that touches it is that
/// final overwriting rename, so an interrupt at any point leaves either the
/// old or the new database there, never nothing and never a half-written
/// file. (Moving the old database aside *before* the staged copy was in place
/// left a window in which a kill produced no database at all — the next
/// `serve` then created and migrated an empty one and served it.)
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

    // Decided before the checkpoint opens a pool: opening one creates an
    // empty database where there was none, which is nothing to keep aside.
    let kept = db_path.exists();

    let sidecars = checkpoint_and_list_sidecars(config_dir, cfg, db_path)?;

    let spin = Spinner::new("Restoring database...");

    let staged = db_path.with_extension("db.restore-tmp");
    fs::copy(backup_dir.join("crap.db"), &staged)
        .with_context(|| format!("Failed to stage database copy at {}", staged.display()))?;

    let aside = db_path.with_extension("db.pre-restore");
    if kept {
        keep_previous_database(db_path, &aside)?;
    }

    remove_sidecars(&sidecars)?;

    fs::rename(&staged, db_path)
        .with_context(|| format!("Failed to move restored database to {}", db_path.display()))?;

    spin.finish_success("Database restored");

    if kept {
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
) -> Result<Vec<PathBuf>> {
    let pool = pool::create_pool(config_dir, cfg).context("Failed to create database pool")?;
    let conn = pool.get().context("Failed to get DB connection")?;

    if !conn.is_sqlite() {
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

/// Extract the backup's uploads only when this project keeps its uploads in
/// itself. Uploads held in another storage are restored with that service —
/// extracting them into `<config>/uploads` would drop files nothing ever reads
/// and report it as a success. Mirrors what `backup` skips on the way out.
fn restore_local_uploads(cfg: &CrapConfig, config_dir: &Path, backup_dir: &Path) -> Result<()> {
    let storage = cfg.upload.storage;

    if !matches!(storage, UploadStorage::Local) {
        cli::info(&format!(
            "Uploads are kept in {storage:?} storage, not in this project — restore them \
             with that service. Skipping."
        ));
        return Ok(());
    }

    restore_uploads(config_dir, backup_dir)
}

/// Extract the uploads.tar.gz archive from the backup directory. A backup with
/// no uploads archive is not an error (uploads were never backed up); a `tar`
/// failure is — the caller requested uploads and they did not restore, so the
/// whole `restore` must not report success. The database has already been
/// restored at this point, which the error message makes clear.
#[cfg(not(tarpaulin_include))]
fn restore_uploads(config_dir: &Path, backup_dir: &Path) -> Result<()> {
    let archive_path = backup_dir.join("uploads.tar.gz");

    if !archive_path.exists() {
        cli::info("No uploads.tar.gz in backup — skipping uploads restore.");
        return Ok(());
    }

    let spin = Spinner::new("Extracting uploads...");

    // Extract ONLY the `uploads` member the backup wrote: an archive carrying
    // `init.lua`, `hooks/`, or `crap.toml` must not be able to overwrite
    // operator code. `--no-same-owner` keeps ownership at the restoring user.
    let status = process::Command::new("tar")
        .args([
            "xzf",
            &archive_path.to_string_lossy(),
            "--no-same-owner",
            "-C",
            &config_dir.to_string_lossy(),
            "uploads",
        ])
        .status();

    match classify_tar_status(status) {
        Ok(()) => {
            spin.finish_success("Uploads restored");
            Ok(())
        }
        Err(e) => {
            spin.finish_warning(&e.to_string());
            Err(e).context("Uploads restore failed — the database was already restored")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: `--include-uploads` extracted the archive into
    /// `<config>/uploads` and reported "Uploads restored" whatever
    /// `[upload] storage` said, so on S3/custom storage it was a silent
    /// no-op dressed up as success. `backup` already skips those with a
    /// note; restore now mirrors it.
    #[test]
    fn uploads_kept_in_another_storage_are_not_extracted_into_the_project() {
        let config_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        // Not a real archive: reaching `tar` at all would fail the call, so
        // an `Ok` here can only mean the skip happened.
        fs::write(backup_dir.path().join("uploads.tar.gz"), b"not-an-archive").unwrap();

        let mut cfg = CrapConfig::default();
        cfg.upload.storage = UploadStorage::S3;

        restore_local_uploads(&cfg, config_dir.path(), backup_dir.path()).unwrap();

        assert!(
            !config_dir.path().join("uploads").exists(),
            "nothing may be extracted for uploads kept in another storage"
        );
    }

    /// Regression: the previous database was renamed aside *before* the
    /// staged copy was moved in, so a kill between the two left no database
    /// at all and the next `serve` created and migrated an empty one. The
    /// live path must still hold a database while the copy is taken.
    #[test]
    fn the_live_database_stays_in_place_while_the_previous_copy_is_taken() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("crap.db");
        fs::write(&db_path, b"old database").unwrap();
        let aside = db_path.with_extension("db.pre-restore");

        keep_previous_database(&db_path, &aside).unwrap();

        assert!(
            db_path.exists(),
            "the live database must never be unlinked to make the copy"
        );
        assert_eq!(fs::read(&aside).unwrap(), b"old database");
    }

    /// The swap itself: one overwriting rename publishes the restored file,
    /// and the kept copy still holds the previous bytes afterwards (it names
    /// the old inode, which the rename does not touch).
    #[test]
    fn the_staged_copy_replaces_the_database_without_losing_the_kept_one() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("crap.db");
        fs::write(&db_path, b"old database").unwrap();
        let aside = db_path.with_extension("db.pre-restore");
        let staged = db_path.with_extension("db.restore-tmp");
        fs::write(&staged, b"restored database").unwrap();

        keep_previous_database(&db_path, &aside).unwrap();
        fs::rename(&staged, &db_path).unwrap();

        assert_eq!(fs::read(&db_path).unwrap(), b"restored database");
        assert_eq!(
            fs::read(&aside).unwrap(),
            b"old database",
            "the kept copy must survive the overwriting rename"
        );
        assert!(!staged.exists(), "the staging file must not linger");
    }

    /// A `*.pre-restore` left by an earlier restore is replaced, not kept —
    /// the operator's fallback must be the database that was just replaced.
    #[test]
    fn an_earlier_previous_copy_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("crap.db");
        fs::write(&db_path, b"current").unwrap();
        let aside = db_path.with_extension("db.pre-restore");
        fs::write(&aside, b"from a restore two weeks ago").unwrap();

        keep_previous_database(&db_path, &aside).unwrap();

        assert_eq!(fs::read(&aside).unwrap(), b"current");
    }

    /// Regression: the checkpoint opened a pool before the restore looked
    /// whether a database existed, and opening the pool creates an empty one
    /// — so restoring into a project without a database kept an empty
    /// `crap.db.pre-restore` and announced it as the previous database.
    #[test]
    fn restoring_into_a_project_without_a_database_keeps_nothing_aside() {
        let config_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        fs::write(backup_dir.path().join("crap.db"), b"restored database").unwrap();

        let mut cfg = CrapConfig::default();
        cfg.database.path = "crap.db".to_string();
        let db_path = cfg.db_path(config_dir.path());
        assert!(!db_path.exists(), "the project starts without a database");

        restore_database(config_dir.path(), &cfg, backup_dir.path(), &db_path).unwrap();

        assert_eq!(fs::read(&db_path).unwrap(), b"restored database");
        assert!(
            !db_path.with_extension("db.pre-restore").exists(),
            "there was no previous database to keep"
        );
    }

    /// The positive control: a project WITH a database keeps it aside.
    #[test]
    fn restoring_over_a_database_keeps_it_aside() {
        let config_dir = tempfile::tempdir().unwrap();
        let backup_dir = tempfile::tempdir().unwrap();
        fs::write(backup_dir.path().join("crap.db"), b"restored database").unwrap();

        let mut cfg = CrapConfig::default();
        cfg.database.path = "crap.db".to_string();
        let db_path = cfg.db_path(config_dir.path());
        let pool = pool::create_pool(config_dir.path(), &cfg).unwrap();
        drop(pool);
        assert!(db_path.exists(), "the pool created the previous database");

        restore_database(config_dir.path(), &cfg, backup_dir.path(), &db_path).unwrap();

        assert_eq!(fs::read(&db_path).unwrap(), b"restored database");
        assert!(
            db_path.with_extension("db.pre-restore").exists(),
            "the previous database is kept aside"
        );
    }

    /// Sidecars are removed before the swap; one already gone is not an error
    /// (a checkpoint or the OS can drop it between listing and removal).
    #[test]
    fn sidecar_removal_tolerates_one_that_is_already_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let wal = tmp.path().join("crap.db-wal");
        let shm = tmp.path().join("crap.db-shm");
        fs::write(&wal, b"wal").unwrap();

        remove_sidecars(&[wal.clone(), shm]).unwrap();

        assert!(!wal.exists());
    }
}
