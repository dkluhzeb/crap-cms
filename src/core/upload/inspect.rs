//! What an upload is before any byte of it is stored: validated, named and
//! measured.
//!
//! Everything here reads the uploaded bytes and nothing writes them, so it is
//! cheap enough to run before the write's access check — which can then judge
//! the file's own columns — while the expensive half (storing the original,
//! every resize, the synchronous format conversions) waits until the check has
//! passed.

use std::collections::HashMap;

use anyhow::Result;

use super::{
    exif::upright_dimensions,
    stored_name::stored_name,
    validate::{check_image_dimensions, decodable_image, validate_upload},
};
use crate::core::upload::{CollectionUpload, UploadedFile};

/// The server-derived columns a file determines before it is stored: what
/// every write of it records, and all a check made before storing can show a
/// rule. The `url` and per-size columns only exist once the file is stored.
#[derive(Debug, Clone)]
pub struct FileColumns {
    /// The name the file is stored under, `{id}_{sanitized original}`.
    pub filename: String,
    /// The validated content type — sniffed from the bytes when they are
    /// recognisable, the (concrete) claimed one otherwise.
    pub mime_type: String,
    pub filesize: u64,
    /// The upright width of a decodable image (EXIF orientation applied).
    pub width: Option<u32>,
    /// The upright height of a decodable image (EXIF orientation applied).
    pub height: Option<u32>,
}

impl FileColumns {
    /// Write these columns into `form`. Every server-derived column of
    /// `upload` is blanked first, so the ones this file does not produce are
    /// written as explicit blanks — which the write edge coerces to NULL —
    /// rather than inherited from a previous file.
    pub fn inject(&self, form: &mut HashMap<String, String>, upload: &CollectionUpload) {
        for name in upload.derived_field_names() {
            form.insert(name, String::new());
        }

        form.insert("filename".into(), self.filename.clone());
        form.insert("mime_type".into(), self.mime_type.clone());
        form.insert("filesize".into(), self.filesize.to_string());

        if let Some(width) = self.width {
            form.insert("width".into(), width.to_string());
        }

        if let Some(height) = self.height {
            form.insert("height".into(), height.to_string());
        }
    }
}

/// One uploaded file after validation and before storage.
pub struct InspectedUpload<'a> {
    pub(super) file: &'a UploadedFile,
    pub(super) columns: FileColumns,
}

impl InspectedUpload<'_> {
    /// The columns the stored file will carry.
    #[must_use]
    pub fn columns(&self) -> &FileColumns {
        &self.columns
    }

    /// Whether the file enters the pixel pipeline (resize, format variants).
    pub(super) fn is_decodable_image(&self) -> bool {
        self.columns.width.is_some()
    }
}

/// The upright dimensions of a file of `mime_type`, when it is an image this
/// build decodes. The decompression-bomb check reads them from the header, so
/// a file it refuses is refused here, before anything is stored.
fn measure(mime_type: &str, data: &[u8]) -> Result<Option<(u32, u32)>> {
    if !decodable_image(mime_type) {
        return Ok(None);
    }

    let declared = check_image_dimensions(data)?;

    Ok(Some(upright_dimensions(data, declared)))
}

/// Validate `file` against `upload_config` (type, magic bytes, extension, SVG
/// content, size, decompression-bomb limits) and derive the columns it will be
/// stored with — without storing anything.
///
/// # Errors
///
/// Returns an error naming why the file is refused.
pub fn inspect_upload<'a>(
    file: &'a UploadedFile,
    upload_config: &CollectionUpload,
    global_max_file_size: u64,
) -> Result<InspectedUpload<'a>> {
    let mime_type = validate_upload(file, upload_config, global_max_file_size)?;
    let dimensions = measure(&mime_type, &file.data)?;

    let columns = FileColumns {
        filename: stored_name(&file.filename),
        mime_type,
        filesize: file.data.len() as u64,
        width: dimensions.map(|(w, _)| w),
        height: dimensions.map(|(_, h)| h),
    };

    Ok(InspectedUpload { file, columns })
}

#[cfg(test)]
mod tests {
    use image::{ExtendedColorType, ImageBuffer, ImageEncoder, Rgba, codecs::png::PngEncoder};

    use super::*;
    use crate::core::upload::{
        ImageSizeBuilder, STORED_ID_LEN, exif::jpeg_with_orientation, original_filename,
    };

    const DEFAULT_MAX: u64 = 50 * 1024 * 1024;

    /// A PNG header declaring a 1x1 image — recognisable magic bytes, no body.
    const PNG_HEADER: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90wS\xde";

    fn png(width: u32, height: u32) -> Vec<u8> {
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_fn(width, height, |_, _| Rgba([10, 20, 128, 255]));

        let mut buf = Vec::new();
        PngEncoder::new(&mut buf)
            .write_image(img.as_raw(), width, height, ExtendedColorType::Rgba8)
            .expect("encode PNG");

        buf
    }

    fn uploaded(filename: &str, content_type: &str, data: Vec<u8>) -> UploadedFile {
        UploadedFile {
            filename: filename.to_string(),
            content_type: content_type.to_string(),
            data,
        }
    }

    fn images_only() -> CollectionUpload {
        CollectionUpload {
            enabled: true,
            mime_types: vec!["image/*".into()],
            ..Default::default()
        }
    }

    fn refusal(file: &UploadedFile, config: &CollectionUpload, max: u64) -> String {
        inspect_upload(file, config, max)
            .err()
            .expect("the file is refused")
            .to_string()
    }

    /// An image's columns are known before it is stored: the stored name, the
    /// detected type, the byte size and the dimensions.
    #[test]
    fn an_image_is_measured_without_being_stored() {
        let file = uploaded("Holiday Photo.PNG", "image/png", png(40, 30));

        let inspected = inspect_upload(&file, &images_only(), DEFAULT_MAX).expect("valid");
        let columns = inspected.columns();

        assert_eq!(
            columns.filename.len(),
            STORED_ID_LEN + 1 + "holiday-photo.png".len()
        );
        assert_eq!(
            original_filename(&columns.filename),
            Some("holiday-photo.png")
        );
        assert_eq!(columns.mime_type, "image/png");
        assert_eq!(columns.filesize, file.data.len() as u64);
        assert_eq!((columns.width, columns.height), (Some(40), Some(30)));
        assert!(inspected.is_decodable_image());
    }

    /// The dimensions are the upright image's — the ones the stored sizes are
    /// cut from — so a photo recorded sideways reports them swapped.
    #[test]
    fn a_rotated_photo_reports_its_upright_dimensions() {
        let file = uploaded("phone.jpg", "image/jpeg", jpeg_with_orientation(8, 4, 6));

        let inspected = inspect_upload(&file, &images_only(), DEFAULT_MAX).expect("valid");

        assert_eq!(
            (inspected.columns().width, inspected.columns().height),
            (Some(4), Some(8))
        );
    }

    /// A file outside the pixel pipeline has no dimensions.
    #[test]
    fn a_non_image_has_no_dimensions() {
        let file = uploaded("doc.pdf", "application/pdf", b"%PDF-1.4 test".to_vec());

        let inspected =
            inspect_upload(&file, &CollectionUpload::default(), DEFAULT_MAX).expect("valid");

        assert_eq!(inspected.columns().mime_type, "application/pdf");
        assert!(inspected.columns().width.is_none());
        assert!(!inspected.is_decodable_image());
    }

    /// The columns written for a file blank every derived column it does not
    /// produce — a previous file's size urls and dimensions are never
    /// inherited.
    #[test]
    fn injected_columns_blank_what_the_file_does_not_produce() {
        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![ImageSizeBuilder::new("thumb").width(10).height(10).build()];

        let columns = FileColumns {
            filename: "abcdefghij_notes.txt".to_string(),
            mime_type: "text/plain".to_string(),
            filesize: 12,
            width: None,
            height: None,
        };

        let mut form = HashMap::from([
            ("width".to_string(), "800".to_string()),
            (
                "thumb_url".to_string(),
                "/uploads/media/old.png".to_string(),
            ),
            ("title".to_string(), "kept".to_string()),
        ]);

        columns.inject(&mut form, &upload);

        assert_eq!(form["filename"], "abcdefghij_notes.txt");
        assert_eq!(form["mime_type"], "text/plain");
        assert_eq!(form["filesize"], "12");
        assert_eq!(form["width"], "");
        assert_eq!(form["url"], "");
        assert_eq!(form["thumb_url"], "");
        assert_eq!(form["title"], "kept", "a user field is not touched");
    }

    #[test]
    fn magic_byte_verification_rejects_mismatched_type() {
        let file = uploaded("evil.txt", "text/plain", PNG_HEADER.to_vec());

        let err = refusal(&file, &CollectionUpload::default(), DEFAULT_MAX);

        assert!(err.contains("does not match claimed type"), "Error: {err}");
    }

    #[test]
    fn magic_byte_verification_allows_matching_type() {
        let file = uploaded("image.png", "image/png", PNG_HEADER.to_vec());

        // A header-only PNG passes the MIME check (anything later may refuse it).
        let err = inspect_upload(&file, &images_only(), DEFAULT_MAX)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();

        assert!(
            !err.contains("does not match claimed type"),
            "Unexpected mismatch: {err}"
        );
    }

    #[test]
    fn magic_byte_verification_passes_text_files() {
        // Plain text has no magic bytes — infer returns None, so it passes through.
        let file = uploaded("readme.txt", "text/plain", b"Hello, world!".to_vec());

        assert!(inspect_upload(&file, &CollectionUpload::default(), DEFAULT_MAX).is_ok());
    }

    /// Regression: the old bidirectional check allowed bypasses where
    /// `mime_matches(claimed, detected)` passed even though
    /// `mime_matches(detected, claimed)` failed. A PNG claimed as
    /// `image/jpeg` must be rejected.
    #[test]
    fn mime_verification_is_one_directional() {
        let file = uploaded("fake.jpg", "image/jpeg", png(10, 10));

        let err = refusal(&file, &images_only(), DEFAULT_MAX);

        assert!(
            err.contains("does not match claimed type"),
            "Error should indicate MIME mismatch: {err}"
        );
    }

    #[test]
    fn a_type_outside_the_allowlist_is_refused() {
        let file = uploaded("test.txt", "text/plain", b"hello".to_vec());

        let err = refusal(&file, &images_only(), DEFAULT_MAX);

        assert!(
            err.contains("text/plain"),
            "Error should mention the rejected MIME type"
        );
    }

    #[test]
    fn an_oversized_file_is_refused() {
        let file = uploaded("big.bin", "application/octet-stream", vec![0u8; 1024]);
        let config = CollectionUpload {
            enabled: true,
            max_file_size: Some(512),
            ..Default::default()
        };

        assert!(refusal(&file, &config, DEFAULT_MAX).contains("exceeds"));
    }

    #[test]
    fn the_global_max_applies_without_a_collection_limit() {
        let file = uploaded("big.bin", "application/octet-stream", vec![0u8; 1024]);

        assert!(refusal(&file, &CollectionUpload::default(), 512).contains("exceeds"));
    }

    /// The sanitising path is what makes storing SVGs safe, so it runs on the
    /// way in: a scripted SVG is refused before anything is stored.
    #[test]
    fn a_scripted_svg_is_refused() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#;
        let file = uploaded("evil.svg", "image/svg+xml", svg.to_vec());

        let err = refusal(&file, &images_only(), DEFAULT_MAX);

        assert!(err.contains("<script>"), "unexpected error: {err}");
    }

    /// An XXE payload is refused on the same path.
    #[test]
    fn an_svg_with_a_doctype_is_refused() {
        let svg = br#"<?xml version="1.0"?>
<!DOCTYPE svg [<!ENTITY xxe SYSTEM "file:///etc/passwd">]>
<svg xmlns="http://www.w3.org/2000/svg"><text>&xxe;</text></svg>"#;
        let file = uploaded("xxe.svg", "image/svg+xml", svg.to_vec());

        assert!(inspect_upload(&file, &images_only(), DEFAULT_MAX).is_err());
    }
}
