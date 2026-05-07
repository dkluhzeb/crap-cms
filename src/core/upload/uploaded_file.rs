//! Raw uploaded file before processing.

/// Raw uploaded file before processing.
pub struct UploadedFile {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}
