//! Point-in-time capture of the local uploads tree for `backup -i`.
//!
//! `backup` runs beside a live `serve`, so the uploads directory keeps
//! changing while it is read: every local-storage write stages a `*.crap-tmp`
//! sibling and renames it away, and deletes remove files a moment after their
//! document is gone. Archiving the live tree directly made GNU `tar` fail
//! ("file changed as we read it") on a busy site, and let a file referenced
//! by the database snapshot disappear before `tar` reached it.
//!
//! Instead the tree is captured into a private staging directory under
//! `data/` (same filesystem in the usual layout, never served, never part of
//! a blueprint), file by file as hard links (a copy where the filesystem refuses links),
//! in two passes that bracket the database snapshot:
//!
//! 1. [`UploadsSnapshot::capture`] before `VACUUM INTO` — every file present
//!    then is pinned: a hard link keeps the inode alive even when `serve`
//!    deletes or replaces the file afterwards (local writes rename a new
//!    inode over the key, they never rewrite one in place).
//! 2. [`UploadsSnapshot::capture`] again after it — adds the files created
//!    while the snapshot ran.
//!
//! The archive is then built from the staging directory, which nothing else
//! writes. Staging files (`*.crap-tmp`) are never captured.
//!
//! The staging directory holds an exclusive lock file for as long as its
//! backup runs. A backup that is killed (Ctrl-C during a long `tar`) never
//! runs its cleanup; the next backup finds that directory with its lock free
//! and removes it, so an abandoned capture does not keep pinning (or, as
//! copies, duplicating) the uploads for good.
//!
//! The one file the capture can miss is one created *after* the first pass
//! read its directory and deleted again before the second pass did, while the
//! database snapshot still referenced it: an upload replaced or hard-deleted
//! within the seconds `VACUUM INTO` takes.

use std::{
    fs::{self, DirEntry, File},
    io::ErrorKind,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow};
use nanoid::nanoid;

use crate::{commands::db::helpers::create_private_dir, core::upload::is_staging_file_name};

/// Name of the uploads directory, both in the project and in the archive.
pub(super) const UPLOADS_DIR: &str = "uploads";

/// Name prefix of the staging directory inside `<config_dir>/data`.
const SNAPSHOT_DIR_PREFIX: &str = ".crap-backup-uploads-";

/// Lock file inside a staging directory, held exclusively by its backup.
const LOCK_FILE: &str = ".lock";

/// A private staging directory holding a captured copy of `uploads/`.
/// Removed on drop.
pub(super) struct UploadsSnapshot {
    source: PathBuf,
    root: PathBuf,
    /// The staging directory's lock, held until drop.
    lock: Option<File>,
}

impl UploadsSnapshot {
    /// Create the staging directory for `<config_dir>/uploads`. `Ok(None)` when
    /// the project has no uploads directory.
    ///
    /// # Errors
    ///
    /// Returns an error when the staging directory cannot be created.
    pub(super) fn create(config_dir: &Path) -> Result<Option<Self>> {
        let source = config_dir.join(UPLOADS_DIR);
        if !source.is_dir() {
            return Ok(None);
        }

        let data_dir = config_dir.join("data");
        remove_abandoned(&data_dir);

        let root = data_dir.join(format!("{SNAPSHOT_DIR_PREFIX}{}", nanoid!(12)));
        create_private_dir(&root)?;

        let mut snapshot = Self {
            source,
            root,
            lock: None,
        };
        snapshot.lock = Some(lock_staging(&snapshot.root)?);
        create_private_dir(&snapshot.captured())?;

        Ok(Some(snapshot))
    }

    /// The directory the archive is built from (`uploads/` lives inside it).
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// The captured `uploads/` tree.
    fn captured(&self) -> PathBuf {
        self.root.join(UPLOADS_DIR)
    }

    /// Capture every file of the live tree not captured yet. Returns how many
    /// files this pass added.
    ///
    /// # Errors
    ///
    /// Returns an error on any filesystem failure other than a file or
    /// directory vanishing mid-walk (a concurrent delete).
    pub(super) fn capture(&self) -> Result<u64> {
        capture_dir(&self.source, &self.captured())
    }
}

impl Drop for UploadsSnapshot {
    fn drop(&mut self) {
        // Close the lock first: some platforms refuse to delete an open file.
        drop(self.lock.take());

        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Create and exclusively lock the staging directory's lock file.
fn lock_staging(root: &Path) -> Result<File> {
    let path = root.join(LOCK_FILE);
    let file =
        File::create(&path).with_context(|| format!("Failed to create {}", path.display()))?;

    file.try_lock()
        .map_err(|e| anyhow!("Failed to lock {}: {e}", path.display()))?;

    Ok(file)
}

/// Remove the staging directories of backups that are no longer running
/// (killed before their cleanup ran). Best effort: a directory whose lock is
/// held belongs to a running backup and is kept, as is one without a lock
/// file (a backup between creating its directory and locking it).
fn remove_abandoned(data_dir: &Path) {
    let Ok(entries) = fs::read_dir(data_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let is_staging = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(SNAPSHOT_DIR_PREFIX));

        if is_staging && is_abandoned(&entry.path()) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Whether the staging directory's lock can be taken — its backup is gone.
fn is_abandoned(root: &Path) -> bool {
    let Ok(lock) = File::open(root.join(LOCK_FILE)) else {
        return false;
    };

    lock.try_lock().is_ok()
}

/// Walk `src` into `dest` without following symlinks.
fn capture_dir(src: &Path, dest: &Path) -> Result<u64> {
    let entries = match fs::read_dir(src) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).with_context(|| format!("Failed to read {}", src.display())),
    };

    let mut added = 0;
    for entry in entries {
        let entry = entry.with_context(|| format!("Failed to read {}", src.display()))?;
        added += capture_entry(&entry, dest)?;
    }

    Ok(added)
}

/// Capture one directory entry: recurse into a directory, pin a regular file,
/// skip staging files, symlinks and anything else.
fn capture_entry(entry: &DirEntry, dest_dir: &Path) -> Result<u64> {
    let name = entry.file_name();
    if is_staging_file_name(&name) {
        return Ok(0);
    }

    let file_type = entry
        .file_type()
        .with_context(|| format!("Failed to stat {}", entry.path().display()))?;
    let dest = dest_dir.join(&name);

    if file_type.is_dir() {
        fs::create_dir_all(&dest)
            .with_context(|| format!("Failed to create {}", dest.display()))?;

        return capture_dir(&entry.path(), &dest);
    }

    if !file_type.is_file() || dest.exists() {
        return Ok(0);
    }

    pin_file(&entry.path(), &dest)
}

/// Hard-link `src` to `dest`, copying when the filesystem refuses the link.
/// A file deleted before it could be pinned is skipped.
fn pin_file(src: &Path, dest: &Path) -> Result<u64> {
    let outcome = fs::hard_link(src, dest).or_else(|e| {
        if e.kind() == ErrorKind::NotFound {
            return Err(e);
        }

        fs::copy(src, dest).map(|_| ())
    });

    match outcome {
        Ok(()) => Ok(1),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e).with_context(|| format!("Failed to capture {}", src.display())),
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use super::*;

    fn project_with_uploads() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join(UPLOADS_DIR).join("media");
        fs::create_dir_all(&media).unwrap();
        fs::write(media.join("a.png"), b"a").unwrap();
        fs::write(media.join(".a.png.123.0.crap-tmp"), b"half").unwrap();
        dir
    }

    #[test]
    fn no_uploads_dir_means_no_snapshot() {
        let dir = tempfile::tempdir().unwrap();

        assert!(UploadsSnapshot::create(dir.path()).unwrap().is_none());
    }

    /// Regression: a file deleted after the first pass (after the database
    /// snapshot referenced it) must still be in the capture.
    #[test]
    fn a_file_deleted_after_capture_stays_captured() {
        let dir = project_with_uploads();
        let snap = UploadsSnapshot::create(dir.path()).unwrap().unwrap();

        assert_eq!(snap.capture().unwrap(), 1);
        fs::remove_file(dir.path().join("uploads/media/a.png")).unwrap();

        let captured = snap.root().join("uploads/media/a.png");
        assert_eq!(fs::read(captured).unwrap(), b"a");
    }

    /// A file replaced after capture (rename over the key) keeps the captured
    /// version; a file created between the passes is added by the second.
    #[test]
    fn second_pass_adds_new_files_and_keeps_pinned_versions() {
        let dir = project_with_uploads();
        let media = dir.path().join("uploads/media");
        let snap = UploadsSnapshot::create(dir.path()).unwrap().unwrap();
        snap.capture().unwrap();

        fs::write(media.join("replacement"), b"new").unwrap();
        fs::rename(media.join("replacement"), media.join("a.png")).unwrap();
        fs::write(media.join("b.png"), b"b").unwrap();

        assert_eq!(snap.capture().unwrap(), 1);
        assert_eq!(
            fs::read(snap.root().join("uploads/media/a.png")).unwrap(),
            b"a"
        );
        assert_eq!(
            fs::read(snap.root().join("uploads/media/b.png")).unwrap(),
            b"b"
        );
    }

    /// Regression: in-flight `*.crap-tmp` staging files made `tar` fail with
    /// "file changed as we read it"; they are never captured.
    #[test]
    fn staging_files_are_not_captured() {
        let dir = project_with_uploads();
        let snap = UploadsSnapshot::create(dir.path()).unwrap().unwrap();
        snap.capture().unwrap();

        let names: Vec<_> = fs::read_dir(snap.root().join("uploads/media"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![OsString::from("a.png")]);
    }

    #[cfg(unix)]
    #[test]
    fn directory_symlinks_are_not_followed() {
        let dir = project_with_uploads();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"s").unwrap();
        symlink(outside.path(), dir.path().join("uploads/link")).unwrap();

        let snap = UploadsSnapshot::create(dir.path()).unwrap().unwrap();
        snap.capture().unwrap();

        assert!(!snap.root().join("uploads/link").exists());
    }

    /// Regression: a backup killed during `tar` (Ctrl-C) never ran its
    /// cleanup, leaving its capture under `data/` for good — hard links
    /// pinning deleted uploads, or a full copy of them. The next backup
    /// removes a staging directory whose lock is free and keeps one whose
    /// backup still runs.
    #[test]
    fn abandoned_staging_dirs_are_removed_by_the_next_backup() {
        let dir = project_with_uploads();
        let running = UploadsSnapshot::create(dir.path()).unwrap().unwrap();

        let abandoned = dir.path().join(format!("data/{SNAPSHOT_DIR_PREFIX}dead"));
        fs::create_dir_all(abandoned.join("uploads")).unwrap();
        fs::write(abandoned.join(LOCK_FILE), b"").unwrap();
        fs::write(abandoned.join("uploads/pinned.png"), b"p").unwrap();

        let unrelated = dir.path().join("data/keep-me");
        fs::create_dir_all(&unrelated).unwrap();

        let next = UploadsSnapshot::create(dir.path()).unwrap().unwrap();

        assert!(!abandoned.exists(), "the abandoned capture must be removed");
        assert!(
            running.root().exists(),
            "a running backup's capture is kept"
        );
        assert!(next.root().exists());
        assert!(unrelated.exists());
    }

    #[test]
    fn staging_dir_is_removed_on_drop() {
        let dir = project_with_uploads();
        let snap = UploadsSnapshot::create(dir.path()).unwrap().unwrap();
        let root = snap.root().to_path_buf();
        drop(snap);

        assert!(!root.exists());
    }
}
