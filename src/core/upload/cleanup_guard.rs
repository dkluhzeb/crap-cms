//! The guard that removes a stored upload's bytes unless its write commits.

use std::fmt;

use tracing::warn;

use crate::core::upload::SharedStorage;

/// RAII guard that deletes written files if not committed.
/// Returned from [`process_upload`](super::process_upload) so callers can
/// commit only after their DB transaction succeeds — preventing orphaned files
/// on rollback.
pub struct CleanupGuard {
    keys: Vec<String>,
    storage: SharedStorage,
    committed: bool,
}

impl fmt::Debug for CleanupGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CleanupGuard")
            .field("keys", &self.keys)
            .field("committed", &self.committed)
            // `storage: SharedStorage` (Arc<dyn StorageBackend>) intentionally
            // omitted — its `Debug` would print backend addresses, not state
            // operators care about.
            .finish_non_exhaustive()
    }
}

impl CleanupGuard {
    pub(super) fn new(storage: SharedStorage) -> Self {
        Self {
            keys: Vec::new(),
            storage,
            committed: false,
        }
    }

    pub(super) fn push(&mut self, key: String) {
        self.keys.push(key);
    }

    /// The storage keys written so far.
    pub(super) fn keys(&self) -> &[String] {
        &self.keys
    }

    /// Mark the guard as committed — files will NOT be cleaned up on drop.
    /// Call this after the database transaction has been committed successfully.
    pub fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }

        for key in &self.keys {
            // Best-effort rollback cleanup; a failure here leaves an orphaned
            // file, so log it rather than swallowing silently (mirrors
            // `delete_upload_files` in metadata.rs).
            if let Err(e) = self.storage.delete(key) {
                warn!("Failed to clean up orphaned upload '{key}' after rollback: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::core::upload::storage::LocalStorage;

    fn test_storage(tmp: &tempfile::TempDir) -> SharedStorage {
        Arc::new(LocalStorage::new(tmp.path().join("uploads")))
    }

    #[test]
    fn cleanup_guard_removes_files_on_drop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);

        storage.put("a.txt", b"a", "text/plain").unwrap();
        storage.put("b.txt", b"b", "text/plain").unwrap();

        {
            let mut guard = CleanupGuard::new(storage.clone());
            guard.push("a.txt".to_string());
            guard.push("b.txt".to_string());
            // guard drops here without commit
        }

        assert!(
            !storage.exists("a.txt").unwrap(),
            "a.txt should be removed on drop"
        );
        assert!(
            !storage.exists("b.txt").unwrap(),
            "b.txt should be removed on drop"
        );
    }

    #[test]
    fn cleanup_guard_keeps_files_on_commit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);

        storage.put("keep.txt", b"keep", "text/plain").unwrap();

        {
            let mut guard = CleanupGuard::new(storage.clone());
            guard.push("keep.txt".to_string());
            guard.commit();
        }

        assert!(
            storage.exists("keep.txt").unwrap(),
            "keep.txt should remain after commit"
        );
    }
}
