use std::collections::HashMap;
use std::path::PathBuf;

use super::processed_upload_builder::ProcessedUploadBuilder;
use super::queued_conversion::QueuedConversion;
use super::size_result::SizeResult;

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
    /// Files created on disk during processing. Used for cleanup if the DB write fails.
    pub created_files: Vec<PathBuf>,
}

impl ProcessedUpload {
    pub fn builder(filename: impl Into<String>, url: impl Into<String>) -> ProcessedUploadBuilder {
        ProcessedUploadBuilder::new(filename, url)
    }

    /// Delete all files created during processing.
    /// Call this when the subsequent DB transaction fails to avoid orphaned files.
    pub fn cleanup(&self) {
        for path in &self.created_files {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_removes_created_files() {
        let tmp = tempfile::tempdir().unwrap();
        let f1 = tmp.path().join("a.txt");
        let f2 = tmp.path().join("b.txt");
        std::fs::write(&f1, b"a").unwrap();
        std::fs::write(&f2, b"b").unwrap();

        let upload = ProcessedUploadBuilder::new("test.jpg", "/uploads/test.jpg")
            .mime_type("image/jpeg")
            .filesize(100)
            .created_files(vec![f1.clone(), f2.clone()])
            .build();

        assert!(f1.exists());
        assert!(f2.exists());
        upload.cleanup();
        assert!(!f1.exists(), "f1 should be deleted after cleanup");
        assert!(!f2.exists(), "f2 should be deleted after cleanup");
    }

    #[test]
    fn cleanup_ignores_already_deleted_files() {
        let tmp = tempfile::tempdir().unwrap();
        let f1 = tmp.path().join("gone.txt");
        // Don't create the file — it doesn't exist

        let upload = ProcessedUploadBuilder::new("test.jpg", "/uploads/test.jpg")
            .mime_type("image/jpeg")
            .filesize(100)
            .created_files(vec![f1])
            .build();

        // Should not panic
        upload.cleanup();
    }
}
