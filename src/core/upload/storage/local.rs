//! Local filesystem storage backend.

use std::{fs, path::PathBuf};

use anyhow::{Context as _, Result, bail};

use super::StorageBackend;

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

/// Strict validation for storage keys. Rejects any input that could, when
/// joined with a base directory, produce a filesystem path outside that
/// base — i.e. path traversal via `..`, absolute paths, or null bytes.
fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() {
        bail!("Storage key is empty");
    }

    if key.contains('\0') {
        bail!("Storage key contains a null byte");
    }

    // Absolute paths (Unix `/` or Windows drive-letter / UNC-style) must be
    // rejected — `PathBuf::join` with an absolute RHS silently replaces the
    // base. Checking the first byte handles both forms portably.
    let first = key.as_bytes()[0];
    if first == b'/' || first == b'\\' {
        bail!("Storage key must be relative: {key:?}");
    }

    // Reject `..` as any component, using both separators so that a key
    // like `foo\..\bar` is caught on filesystems that treat `\` specially.
    for component in key.split(['/', '\\']) {
        if component == ".." {
            bail!("Storage key contains '..' traversal: {key:?}");
        }
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

        fs::write(&path, data)
            .with_context(|| format!("Failed to write file: {}", path.display()))?;

        Ok(())
    }

    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let path = self.key_to_path(key)?;

        fs::read(&path).with_context(|| format!("Failed to read file: {}", path.display()))
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

    fn public_url(&self, key: &str) -> String {
        format!("/uploads/{}", key)
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
    use super::*;

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

    #[test]
    fn public_url() {
        let storage = LocalStorage::new("/tmp/uploads");
        assert_eq!(
            storage.public_url("media/photo.jpg"),
            "/uploads/media/photo.jpg"
        );
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
    // Regression tests for audit finding H-2. The trait is the trust boundary:
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
