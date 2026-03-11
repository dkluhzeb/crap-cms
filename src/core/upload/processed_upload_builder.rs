use std::collections::HashMap;
use std::path::PathBuf;

use super::processed_upload::ProcessedUpload;
use super::queued_conversion::QueuedConversion;
use super::size_result::SizeResult;

/// Builder for [`ProcessedUpload`].
///
/// `filename` and `url` are taken in `new()`. `mime_type` and `filesize` are
/// required via chained methods. Image dimensions and size maps are optional.
pub struct ProcessedUploadBuilder {
    filename: String,
    mime_type: Option<String>,
    filesize: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    url: String,
    sizes: HashMap<String, SizeResult>,
    queued_conversions: Vec<QueuedConversion>,
    created_files: Vec<PathBuf>,
}

impl ProcessedUploadBuilder {
    pub fn new(filename: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            filename: filename.into(),
            url: url.into(),
            mime_type: None,
            filesize: None,
            width: None,
            height: None,
            sizes: HashMap::new(),
            queued_conversions: Vec::new(),
            created_files: Vec::new(),
        }
    }

    pub fn mime_type(mut self, m: impl Into<String>) -> Self {
        self.mime_type = Some(m.into());
        self
    }

    pub fn filesize(mut self, s: u64) -> Self {
        self.filesize = Some(s);
        self
    }

    pub fn width(mut self, w: u32) -> Self {
        self.width = Some(w);
        self
    }

    pub fn height(mut self, h: u32) -> Self {
        self.height = Some(h);
        self
    }

    pub fn sizes(mut self, s: HashMap<String, SizeResult>) -> Self {
        self.sizes = s;
        self
    }

    pub fn queued_conversions(mut self, q: Vec<QueuedConversion>) -> Self {
        self.queued_conversions = q;
        self
    }

    pub fn created_files(mut self, files: Vec<PathBuf>) -> Self {
        self.created_files = files;
        self
    }

    pub fn build(self) -> ProcessedUpload {
        ProcessedUpload {
            filename: self.filename,
            mime_type: self
                .mime_type
                .expect("ProcessedUploadBuilder: mime_type is required"),
            filesize: self
                .filesize
                .expect("ProcessedUploadBuilder: filesize is required"),
            width: self.width,
            height: self.height,
            url: self.url,
            sizes: self.sizes,
            queued_conversions: self.queued_conversions,
            created_files: self.created_files,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_processed_upload_with_required_fields() {
        let upload = ProcessedUploadBuilder::new("image.jpg", "/uploads/image.jpg")
            .mime_type("image/jpeg")
            .filesize(102400)
            .width(1920)
            .height(1080)
            .build();
        assert_eq!(upload.filename, "image.jpg");
        assert_eq!(upload.url, "/uploads/image.jpg");
        assert_eq!(upload.mime_type, "image/jpeg");
        assert_eq!(upload.filesize, 102400);
        assert_eq!(upload.width, Some(1920));
        assert_eq!(upload.height, Some(1080));
        assert!(upload.sizes.is_empty());
        assert!(upload.queued_conversions.is_empty());
    }

    #[test]
    #[should_panic(expected = "ProcessedUploadBuilder: mime_type is required")]
    fn panics_without_mime_type() {
        ProcessedUploadBuilder::new("f.jpg", "/u/f.jpg")
            .filesize(1)
            .build();
    }

    #[test]
    #[should_panic(expected = "ProcessedUploadBuilder: filesize is required")]
    fn panics_without_filesize() {
        ProcessedUploadBuilder::new("f.jpg", "/u/f.jpg")
            .mime_type("image/jpeg")
            .build();
    }
}
