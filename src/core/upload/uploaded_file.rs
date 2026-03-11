use super::uploaded_file_builder::UploadedFileBuilder;

/// Raw uploaded file before processing.
pub struct UploadedFile {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

impl UploadedFile {
    pub fn builder(
        filename: impl Into<String>,
        content_type: impl Into<String>,
    ) -> UploadedFileBuilder {
        UploadedFileBuilder::new(filename, content_type)
    }
}
