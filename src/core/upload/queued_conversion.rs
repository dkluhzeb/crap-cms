//! Deferred format conversion for the image processing queue.

/// A deferred format conversion to be inserted into the image processing queue.
#[derive(Debug, Clone)]
pub struct QueuedConversion {
    pub source_path: String,
    pub target_path: String,
    pub format: String,
    pub quality: u8,
    pub url_column: String,
    pub url_value: String,
}
