//! Result of processing an upload (original + generated sizes/formats).

use std::collections::HashMap;

use crate::core::upload::{QueuedConversion, SizeResult, StorageBackend};

/// Result of processing an upload (original + generated sizes/formats).
#[derive(Debug)]
pub struct ProcessedUpload {
    pub filename: String,
    pub mime_type: String,
    pub filesize: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub url: String,
    pub sizes: HashMap<String, SizeResult>,
    /// Format conversions deferred to the background queue (when per-format `queue = true`).
    pub queued_conversions: Vec<QueuedConversion>,
    /// Storage keys created during processing. Used for cleanup if the DB write fails.
    pub created_files: Vec<String>,
}

impl ProcessedUpload {
    /// Delete all files created during processing via the storage backend.
    /// Call this when the subsequent DB transaction fails to avoid orphaned files.
    pub fn cleanup(&self, storage: &dyn StorageBackend) {
        for key in &self.created_files {
            let _ = storage.delete(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::upload::storage::LocalStorage;

    #[test]
    fn cleanup_removes_created_files() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = LocalStorage::new(tmp.path());

        storage.put("a.txt", b"a", "text/plain").unwrap();
        storage.put("b.txt", b"b", "text/plain").unwrap();

        let upload = ProcessedUpload {
            filename: "test.jpg".to_string(),
            mime_type: "image/jpeg".to_string(),
            filesize: 100,
            width: None,
            height: None,
            url: "/uploads/test.jpg".to_string(),
            sizes: HashMap::new(),
            queued_conversions: Vec::new(),
            created_files: vec!["a.txt".to_string(), "b.txt".to_string()],
        };

        assert!(storage.exists("a.txt").unwrap());
        assert!(storage.exists("b.txt").unwrap());
        upload.cleanup(&storage);
        assert!(!storage.exists("a.txt").unwrap());
        assert!(!storage.exists("b.txt").unwrap());
    }

    #[test]
    fn cleanup_ignores_already_deleted_files() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = LocalStorage::new(tmp.path());

        let upload = ProcessedUpload {
            filename: "test.jpg".to_string(),
            mime_type: "image/jpeg".to_string(),
            filesize: 100,
            width: None,
            height: None,
            url: "/uploads/test.jpg".to_string(),
            sizes: HashMap::new(),
            queued_conversions: Vec::new(),
            created_files: vec!["gone.txt".to_string()],
        };

        // Should not panic
        upload.cleanup(&storage);
    }
}
