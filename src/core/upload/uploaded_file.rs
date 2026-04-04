//! Raw uploaded file before processing.

/// Raw uploaded file before processing.
pub struct UploadedFile {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

impl UploadedFile {
    /// Start building a new `UploadedFile`.
    pub fn builder(
        filename: impl Into<String>,
        content_type: impl Into<String>,
    ) -> UploadedFileBuilder {
        UploadedFileBuilder::new(filename, content_type)
    }
}

/// Builder for [`UploadedFile`].
///
/// `filename` and `content_type` are taken in `new()`. `data` is required via
/// a chained method.
pub struct UploadedFileBuilder {
    filename: String,
    content_type: String,
    data: Option<Vec<u8>>,
}

impl UploadedFileBuilder {
    /// Create a new builder with required `filename` and `content_type`.
    pub fn new(filename: impl Into<String>, content_type: impl Into<String>) -> Self {
        Self {
            filename: filename.into(),
            content_type: content_type.into(),
            data: None,
        }
    }

    pub fn data(mut self, d: Vec<u8>) -> Self {
        self.data = Some(d);
        self
    }

    /// Build the final [`UploadedFile`].
    pub fn build(self) -> UploadedFile {
        UploadedFile {
            filename: self.filename,
            content_type: self.content_type,
            data: self.data.expect("UploadedFileBuilder: data is required"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_uploaded_file() {
        let file = UploadedFileBuilder::new("photo.jpg", "image/jpeg")
            .data(vec![0xFF, 0xD8, 0xFF])
            .build();
        assert_eq!(file.filename, "photo.jpg");
        assert_eq!(file.content_type, "image/jpeg");
        assert_eq!(file.data, vec![0xFF, 0xD8, 0xFF]);
    }

    #[test]
    #[should_panic(expected = "UploadedFileBuilder: data is required")]
    fn panics_without_data() {
        UploadedFileBuilder::new("f.txt", "text/plain").build();
    }
}
