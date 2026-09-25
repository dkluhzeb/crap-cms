//! Local filesystem storage backend.

use std::{
    ffi::OsStr,
    fs::{self, File},
    io::{self, Write as _},
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

use anyhow::{Context as _, Result, bail};

use super::backend::validate_key;
use super::{StorageBackend, StorageNotFound};

/// Extension of the sibling file [`write_atomically`] stages bytes in. The
/// name is dot-prefixed and suffixed so a leftover from a killed process is
/// recognisable on sight; it is never a storage key, so no read can reach it.
const TEMP_SUFFIX: &str = ".crap-tmp";

/// Whether a file name is a local-storage staging file (a write in flight or
/// a leftover from a killed process) rather than a stored object. Tools that
/// copy the uploads tree (`backup`, `blueprint save`) skip these.
#[must_use]
pub fn is_staging_file_name(name: &OsStr) -> bool {
    name.to_string_lossy().ends_with(TEMP_SUFFIX)
}

/// Local filesystem storage backend.
///
/// Files are stored under `{base_dir}/{key}`. Directories are created
/// automatically. This is the default backend matching the original behavior.
pub struct LocalStorage {
    base_dir: PathBuf,
}

impl LocalStorage {
    /// Create a new local storage backend rooted at `base_dir`.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    /// Resolve a storage key to an absolute filesystem path under `base_dir`.
    ///
    /// Rejects keys that could escape the base directory: `..` components,
    /// absolute paths, null bytes, or backslash separators (which on
    /// Windows, and via filesystems mounted on Unix, could also act as
    /// directory separators). Callers such as upload handlers already
    /// sanitize filenames before reaching this point — this guard makes
    /// the invariant enforced at the storage boundary so future callers
    /// (Lua hooks, new handlers, migrations) cannot accidentally escape.
    fn key_to_path(&self, key: &str) -> Result<PathBuf> {
        validate_key(key)?;

        let path = self.base_dir.join(key);

        // Belt-and-braces: the component validation above already prevents
        // lexical escape, but re-check against `base_dir` so a future refactor
        // that weakens validation still cannot produce a path outside the
        // root. `starts_with` on `PathBuf` is a component-wise check.
        if !path.starts_with(&self.base_dir) {
            bail!("Storage key escapes base_dir: {key:?}");
        }

        Ok(path)
    }
}

/// A staging path next to `path`, unique within this process and carrying the
/// pid so two processes writing the same key cannot share one.
fn temp_sibling(path: &Path, parent: &Path) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("object"))
        .to_string_lossy();

    parent.join(format!(".{name}.{}.{seq}{TEMP_SUFFIX}", process::id()))
}

/// Put the bytes in `temp`, flush them to disk, and move `temp` onto `path`.
/// Separated from the cleanup in [`write_atomically`] so every failure leaves
/// through one place.
fn fill_and_rename(temp: &Path, path: &Path, data: &[u8]) -> Result<()> {
    let mut file = File::create(temp)
        .with_context(|| format!("Failed to create staging file: {}", temp.display()))?;

    file.write_all(data)
        .with_context(|| format!("Failed to write file: {}", path.display()))?;

    // Flush before the rename. The rename publishes the directory entry
    // atomically, but unflushed bytes can still be in flight — a power loss
    // between the two would publish a key holding zeros.
    file.sync_all()
        .with_context(|| format!("Failed to flush file: {}", path.display()))?;

    drop(file);

    fs::rename(temp, path).with_context(|| format!("Failed to write file: {}", path.display()))
}

/// Write `data` to `path` so a reader only ever sees a complete object.
///
/// A plain write truncates the target first, so a kill mid-write leaves a
/// short file under the final key that later serves as a corrupt download.
/// Staging in a sibling and renaming over the key means the key holds either
/// the previous object or the whole new one. Overwriting an existing key stays
/// allowed — the same contract the S3 backend's `put` offers.
fn write_atomically(path: &Path, data: &[u8]) -> Result<()> {
    let Some(parent) = path.parent() else {
        bail!("Storage path has no parent directory: {}", path.display());
    };

    let temp = temp_sibling(path, parent);

    if let Err(e) = fill_and_rename(&temp, path, data) {
        // Never leave a half-written sibling behind.
        let _ = fs::remove_file(&temp);
        return Err(e);
    }

    // Persist the directory entry itself so the rename survives a crash.
    // Best-effort: not every platform allows opening a directory for sync.
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }

    Ok(())
}

impl StorageBackend for LocalStorage {
    fn put(&self, key: &str, data: &[u8], _content_type: &str) -> Result<()> {
        let path = self.key_to_path(key)?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        write_atomically(&path, data)
    }

    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let path = self.key_to_path(key)?;

        match fs::read(&path) {
            Ok(data) => Ok(data),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err(StorageNotFound(key.to_string()).into())
            }
            Err(e) => Err(e).with_context(|| format!("Failed to read file: {}", path.display())),
        }
    }

    fn delete(&self, key: &str) -> Result<()> {
        let path = self.key_to_path(key)?;

        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("Failed to delete file: {}", path.display()))?;
        }

        Ok(())
    }

    fn exists(&self, key: &str) -> Result<bool> {
        // An invalid key definitionally cannot map to a stored object, so
        // return false rather than propagating — matches the semantics of
        // `exists` (membership query, not a fatal-error operation).
        match self.key_to_path(key) {
            Ok(path) => Ok(path.exists()),
            Err(_) => Ok(false),
        }
    }

    fn kind(&self) -> &'static str {
        "local"
    }

    fn local_path(&self, key: &str) -> Option<PathBuf> {
        self.key_to_path(key).ok()
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    #[test]
    fn staging_names_are_recognised() {
        let staged = temp_sibling(Path::new("/u/media/a.png"), Path::new("/u/media"));

        assert!(is_staging_file_name(staged.file_name().unwrap()));
        assert!(!is_staging_file_name(OsStr::new("a.png")));
    }

    #[test]
    fn put_get_delete() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = LocalStorage::new(tmp.path());

        storage
            .put("media/test.txt", b"hello world", "text/plain")
            .unwrap();
        assert!(tmp.path().join("media/test.txt").exists());

        let data = storage.get("media/test.txt").unwrap();
        assert_eq!(data, b"hello world");

        assert!(storage.exists("media/test.txt").unwrap());
        assert!(!storage.exists("media/nonexistent.txt").unwrap());

        storage.delete("media/test.txt").unwrap();
        assert!(!tmp.path().join("media/test.txt").exists());

        // Delete non-existent is OK
        storage.delete("media/test.txt").unwrap();
    }

    /// Staging files left in `dir` — a successful or cleanly failed `put`
    /// must leave none.
    fn temp_files_in(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.to_string_lossy().ends_with(TEMP_SUFFIX))
            .collect()
    }

    /// Regression: `put` was a plain write, which truncates the target before
    /// writing — a kill mid-write left a short file under the final key that
    /// later served as a corrupt download. The bytes now land in a sibling
    /// and are renamed over the key, so the key holds either the previous
    /// object or the complete new one, and no staging file survives.
    #[test]
    fn put_publishes_the_whole_object_and_keeps_no_staging_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = LocalStorage::new(tmp.path());

        storage.put("media/t.txt", b"first", "text/plain").unwrap();
        storage
            .put("media/t.txt", b"second and longer", "text/plain")
            .unwrap();

        assert_eq!(storage.get("media/t.txt").unwrap(), b"second and longer");
        assert!(
            temp_files_in(&tmp.path().join("media")).is_empty(),
            "no staging file may survive a successful put"
        );
    }

    /// A put that cannot be written leaves nothing behind: no final key (so a
    /// later read is a clean miss rather than a truncated file) and no
    /// staging sibling.
    #[cfg(unix)]
    #[test]
    fn a_failed_put_leaves_neither_the_key_nor_a_staging_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = LocalStorage::new(tmp.path());

        let dir = tmp.path().join("media");
        fs::create_dir_all(&dir).unwrap();
        // Mode 0o555: listable but not writable, so the staging file cannot
        // be created.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();

        let result = storage.put("media/t.txt", b"payload", "text/plain");

        let leftovers = temp_files_in(&dir);
        let key_exists = dir.join("t.txt").exists();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(result.is_err(), "an unwritable directory must fail the put");
        assert!(!key_exists, "the key must not appear when the put failed");
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn creates_directories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = LocalStorage::new(tmp.path());

        storage
            .put("deep/nested/dir/file.txt", b"data", "text/plain")
            .unwrap();
        assert!(tmp.path().join("deep/nested/dir/file.txt").exists());
    }

    // ── Path traversal rejection ──────────────────────────────────────────
    //
    // The trait is the trust boundary:
    // any caller (admin handlers, Lua hooks, future migrations) that hands the
    // backend an attacker-controlled key must not be able to escape `base_dir`.

    #[test]
    fn rejects_parent_traversal() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = LocalStorage::new(tmp.path());

        assert!(storage.put("../escape.txt", b"x", "text/plain").is_err());
        assert!(
            storage
                .put("a/../../escape.txt", b"x", "text/plain")
                .is_err()
        );
        assert!(storage.get("../escape.txt").is_err());
        assert!(storage.delete("../escape.txt").is_err());
    }

    /// A missing key must surface as a typed `StorageNotFound` so the serve
    /// handler can answer 404 (vs 503 for a transient/infra failure).
    #[test]
    fn get_missing_key_returns_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = LocalStorage::new(tmp.path());

        let err = storage.get("media/nope.txt").unwrap_err();
        assert!(
            err.downcast_ref::<StorageNotFound>().is_some(),
            "missing key must map to StorageNotFound, got: {err:#}"
        );
    }

    /// An invalid key (path traversal) is rejected as a transient/validation
    /// error, NOT classified as a not-found (it must not become a 404).
    #[test]
    fn get_invalid_key_is_not_storage_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = LocalStorage::new(tmp.path());

        let err = storage.get("../escape.txt").unwrap_err();
        assert!(err.downcast_ref::<StorageNotFound>().is_none());
    }

    #[test]
    fn rejects_parent_traversal_via_backslash() {
        // On Unix `\` is just a character, but if the file is later opened
        // by a tool that treats `\` as a separator (rsync, some SMB clients)
        // the traversal would succeed. Reject at the storage boundary.
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = LocalStorage::new(tmp.path());

        assert!(
            storage
                .put("foo\\..\\escape.txt", b"x", "text/plain")
                .is_err()
        );
    }

    #[test]
    fn rejects_absolute_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = LocalStorage::new(tmp.path());

        assert!(storage.put("/etc/passwd", b"x", "text/plain").is_err());
        assert!(
            storage
                .put("\\absolute\\win.txt", b"x", "text/plain")
                .is_err()
        );
    }

    #[test]
    fn rejects_empty_and_null_byte_keys() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = LocalStorage::new(tmp.path());

        assert!(storage.put("", b"x", "text/plain").is_err());
        assert!(storage.put("ok\0hidden", b"x", "text/plain").is_err());
    }

    #[test]
    fn exists_returns_false_for_invalid_keys_rather_than_erroring() {
        // `exists` is a membership query — an invalid key simply means
        // "not a stored object", not a hard failure.
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = LocalStorage::new(tmp.path());

        assert!(!storage.exists("../escape.txt").unwrap());
        assert!(!storage.exists("").unwrap());
    }

    #[test]
    fn local_path_returns_none_for_invalid_keys() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = LocalStorage::new(tmp.path());

        assert!(storage.local_path("../escape.txt").is_none());
        assert!(storage.local_path("/etc/passwd").is_none());
        // Legitimate key still resolves.
        assert!(storage.local_path("media/file.png").is_some());
    }
}
