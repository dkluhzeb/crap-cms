//! Local filesystem storage backend.

use std::{fs, path::PathBuf};

use anyhow::{Context as _, Result};

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

    /// Resolve a key to a filesystem path.
    fn key_to_path(&self, key: &str) -> PathBuf {
        self.base_dir.join(key)
    }
}

impl StorageBackend for LocalStorage {
    fn put(&self, key: &str, data: &[u8], _content_type: &str) -> Result<()> {
        let path = self.key_to_path(key);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        fs::write(&path, data)
            .with_context(|| format!("Failed to write file: {}", path.display()))?;

        Ok(())
    }

    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let path = self.key_to_path(key);

        fs::read(&path).with_context(|| format!("Failed to read file: {}", path.display()))
    }

    fn delete(&self, key: &str) -> Result<()> {
        let path = self.key_to_path(key);

        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("Failed to delete file: {}", path.display()))?;
        }

        Ok(())
    }

    fn exists(&self, key: &str) -> Result<bool> {
        Ok(self.key_to_path(key).exists())
    }

    fn public_url(&self, key: &str) -> String {
        format!("/uploads/{}", key)
    }

    fn kind(&self) -> &'static str {
        "local"
    }

    fn local_path(&self, key: &str) -> Option<PathBuf> {
        Some(self.key_to_path(key))
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
}
