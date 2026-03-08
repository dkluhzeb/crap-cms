use std::collections::HashMap;

use super::queued_conversion::QueuedConversion;
use super::size_result::SizeResult;
use super::processed_upload_builder::ProcessedUploadBuilder;

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
}

impl ProcessedUpload {
    pub fn builder(filename: impl Into<String>, url: impl Into<String>) -> ProcessedUploadBuilder {
        ProcessedUploadBuilder::new(filename, url)
    }
}
