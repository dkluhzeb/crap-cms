use std::collections::HashMap;
use std::path::Path;

use super::collection_upload::CollectionUpload;
use super::processed_upload::ProcessedUpload;
use super::queued_conversion::QueuedConversion;

/// Assemble per-size typed columns into a structured `sizes` object on the document.
/// Reads `{name}_url`, `{name}_width`, `{name}_height`, `{name}_webp_url`, `{name}_avif_url`
/// from document fields, builds a nested PayloadCMS-style object, inserts as `sizes`,
/// and removes the individual per-size columns.
pub fn assemble_sizes_object(doc: &mut crate::core::Document, upload: &CollectionUpload) {
    let mut sizes = serde_json::Map::new();

    for size_def in &upload.image_sizes {
        let name = &size_def.name;

        let url = doc
            .fields
            .remove(&format!("{}_url", name))
            .and_then(|v| match v {
                serde_json::Value::String(s) => Some(s),
                _ => None,
            });
        let width = doc
            .fields
            .remove(&format!("{}_width", name))
            .and_then(|v| v.as_f64())
            .map(|v| v as u32);
        let height = doc
            .fields
            .remove(&format!("{}_height", name))
            .and_then(|v| v.as_f64())
            .map(|v| v as u32);

        if let Some(url) = url {
            let mut size_obj = serde_json::Map::new();
            size_obj.insert("url".to_string(), serde_json::Value::String(url));
            if let Some(w) = width {
                size_obj.insert("width".to_string(), serde_json::json!(w));
            }
            if let Some(h) = height {
                size_obj.insert("height".to_string(), serde_json::json!(h));
            }

            let mut formats = serde_json::Map::new();

            if upload.format_options.webp.is_some()
                && let Some(serde_json::Value::String(webp_url)) =
                    doc.fields.remove(&format!("{}_webp_url", name))
            {
                let mut fmt = serde_json::Map::new();
                fmt.insert("url".to_string(), serde_json::Value::String(webp_url));
                formats.insert("webp".to_string(), serde_json::Value::Object(fmt));
            }

            if upload.format_options.avif.is_some()
                && let Some(serde_json::Value::String(avif_url)) =
                    doc.fields.remove(&format!("{}_avif_url", name))
            {
                let mut fmt = serde_json::Map::new();
                fmt.insert("url".to_string(), serde_json::Value::String(avif_url));
                formats.insert("avif".to_string(), serde_json::Value::Object(fmt));
            }

            if !formats.is_empty() {
                size_obj.insert("formats".to_string(), serde_json::Value::Object(formats));
            }

            sizes.insert(name.clone(), serde_json::Value::Object(size_obj));
        } else {
            // Still remove format columns even if there's no URL
            doc.fields.remove(&format!("{}_webp_url", name));
            doc.fields.remove(&format!("{}_avif_url", name));
        }
    }

    if !sizes.is_empty() {
        doc.fields
            .insert("sizes".to_string(), serde_json::Value::Object(sizes));
    }
}

/// Inject upload metadata fields into form data from a processed upload.
/// Writes per-size typed fields ({name}_url, {name}_width, {name}_height, {name}_webp_url, etc.)
pub fn inject_upload_metadata(
    form_data: &mut HashMap<String, String>,
    processed: &ProcessedUpload,
) {
    form_data.insert("filename".into(), processed.filename.clone());
    form_data.insert("mime_type".into(), processed.mime_type.clone());
    form_data.insert("filesize".into(), processed.filesize.to_string());
    if let Some(w) = processed.width {
        form_data.insert("width".into(), w.to_string());
    }
    if let Some(h) = processed.height {
        form_data.insert("height".into(), h.to_string());
    }
    form_data.insert("url".into(), processed.url.clone());

    // Per-size typed fields
    for (name, size) in &processed.sizes {
        form_data.insert(format!("{}_url", name), size.url.clone());
        form_data.insert(format!("{}_width", name), size.width.to_string());
        form_data.insert(format!("{}_height", name), size.height.to_string());
        for (fmt, result) in &size.formats {
            form_data.insert(format!("{}_{}_url", name, fmt), result.url.clone());
        }
    }
}

/// Delete all files associated with an upload document.
/// Reads the url and per-size url fields to determine which files to remove.
pub fn delete_upload_files(config_dir: &Path, doc_fields: &HashMap<String, serde_json::Value>) {
    // Collect all URL fields that point to upload files
    // These are: url, {size}_url, {size}_webp_url, {size}_avif_url
    for (key, value) in doc_fields {
        if (key == "url" || key.ends_with("_url"))
            && !key.contains("image")
            && let serde_json::Value::String(url) = value
            && url.starts_with("/uploads/")
        {
            let rel_path = url.strip_prefix('/').unwrap_or(url);
            let file_path = config_dir.join(rel_path);
            if file_path.exists()
                && let Err(e) = std::fs::remove_file(&file_path)
            {
                tracing::warn!("Failed to delete file {}: {}", file_path.display(), e);
            }
        }
    }
}

/// Insert queued format conversions into the image processing queue.
/// Called after document creation, when the document ID is known.
pub fn enqueue_conversions(
    conn: &rusqlite::Connection,
    collection: &str,
    document_id: &str,
    conversions: &[QueuedConversion],
) -> anyhow::Result<()> {
    use crate::db::query::images::{NewImageEntry, insert_image_queue_entry};
    for c in conversions {
        let entry = NewImageEntry {
            collection,
            document_id,
            source_path: &c.source_path,
            target_path: &c.target_path,
            format: &c.format,
            quality: c.quality,
            url_column: &c.url_column,
            url_value: &c.url_value,
        };
        insert_image_queue_entry(conn, &entry)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::upload::{
        FormatOptions, FormatQuality, FormatResult, ImageSizeBuilder, ProcessedUploadBuilder,
        SizeResultBuilder,
    };

    #[test]
    fn assemble_sizes_builds_structured_object() {
        use crate::core::Document;

        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumbnail")
                .width(300)
                .height(300)
                .build(),
            ImageSizeBuilder::new("card").width(640).height(480).build(),
        ];
        upload.format_options = FormatOptions {
            webp: Some(FormatQuality::new(80, false)),
            avif: None,
        };

        let mut doc = Document::new("test-id".into());
        doc.fields
            .insert("url".into(), serde_json::json!("/uploads/media/orig.png"));
        doc.fields.insert(
            "thumbnail_url".into(),
            serde_json::json!("/uploads/media/thumb.png"),
        );
        doc.fields
            .insert("thumbnail_width".into(), serde_json::json!(300));
        doc.fields
            .insert("thumbnail_height".into(), serde_json::json!(300));
        doc.fields.insert(
            "thumbnail_webp_url".into(),
            serde_json::json!("/uploads/media/thumb.webp"),
        );
        doc.fields.insert(
            "card_url".into(),
            serde_json::json!("/uploads/media/card.png"),
        );
        doc.fields
            .insert("card_width".into(), serde_json::json!(640));
        doc.fields
            .insert("card_height".into(), serde_json::json!(480));
        doc.fields.insert(
            "card_webp_url".into(),
            serde_json::json!("/uploads/media/card.webp"),
        );

        assemble_sizes_object(&mut doc, &upload);

        // Per-size columns should be removed
        assert!(!doc.fields.contains_key("thumbnail_url"));
        assert!(!doc.fields.contains_key("thumbnail_width"));
        assert!(!doc.fields.contains_key("thumbnail_webp_url"));
        assert!(!doc.fields.contains_key("card_url"));

        // url should still be there (it's the original, not a size column)
        assert!(doc.fields.contains_key("url"));

        // sizes should be a structured object
        let sizes = doc.fields.get("sizes").expect("sizes should exist");
        assert!(sizes.is_object());

        let thumb = sizes.get("thumbnail").expect("thumbnail size");
        assert_eq!(
            thumb.get("url").unwrap().as_str().unwrap(),
            "/uploads/media/thumb.png"
        );
        assert_eq!(thumb.get("width").unwrap().as_u64().unwrap(), 300);
        assert_eq!(thumb.get("height").unwrap().as_u64().unwrap(), 300);
        let thumb_formats = thumb.get("formats").expect("formats");
        assert_eq!(
            thumb_formats
                .get("webp")
                .unwrap()
                .get("url")
                .unwrap()
                .as_str()
                .unwrap(),
            "/uploads/media/thumb.webp"
        );

        let card = sizes.get("card").expect("card size");
        assert_eq!(
            card.get("url").unwrap().as_str().unwrap(),
            "/uploads/media/card.png"
        );
        assert_eq!(card.get("width").unwrap().as_u64().unwrap(), 640);
    }

    #[test]
    fn assemble_sizes_empty_when_no_size_columns() {
        use crate::core::Document;

        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumbnail")
                .width(300)
                .height(300)
                .build(),
        ];

        let mut doc = Document::new("test-id".into());
        doc.fields
            .insert("url".into(), serde_json::json!("/uploads/media/orig.pdf"));

        assemble_sizes_object(&mut doc, &upload);

        // No sizes object since no size columns exist
        assert!(!doc.fields.contains_key("sizes"));
        // Original url preserved
        assert!(doc.fields.contains_key("url"));
    }

    #[test]
    fn assemble_sizes_with_avif_format() {
        use crate::core::Document;

        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumb")
                .width(100)
                .height(100)
                .build(),
        ];
        upload.format_options = FormatOptions {
            webp: None,
            avif: Some(FormatQuality::new(50, false)),
        };

        let mut doc = Document::new("id1".into());
        doc.fields
            .insert("thumb_url".into(), serde_json::json!("/uploads/m/t.png"));
        doc.fields
            .insert("thumb_width".into(), serde_json::json!(100));
        doc.fields
            .insert("thumb_height".into(), serde_json::json!(100));
        doc.fields.insert(
            "thumb_avif_url".into(),
            serde_json::json!("/uploads/m/t.avif"),
        );

        assemble_sizes_object(&mut doc, &upload);

        let sizes = doc.fields.get("sizes").expect("sizes should exist");
        let thumb = sizes.get("thumb").expect("thumb");
        let formats = thumb.get("formats").expect("formats");
        assert!(
            formats.get("avif").is_some(),
            "AVIF format should be in assembled object"
        );
        assert_eq!(
            formats
                .get("avif")
                .unwrap()
                .get("url")
                .unwrap()
                .as_str()
                .unwrap(),
            "/uploads/m/t.avif"
        );
        // webp should not be present
        assert!(formats.get("webp").is_none());
    }

    #[test]
    fn assemble_sizes_missing_url_cleans_format_columns() {
        use crate::core::Document;

        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumb")
                .width(100)
                .height(100)
                .build(),
        ];
        upload.format_options = FormatOptions {
            webp: Some(FormatQuality::new(80, false)),
            avif: Some(FormatQuality::new(50, false)),
        };

        let mut doc = Document::new("id1".into());
        // No thumb_url, but format columns exist (edge case: orphaned format columns)
        doc.fields.insert(
            "thumb_webp_url".into(),
            serde_json::json!("/uploads/m/t.webp"),
        );
        doc.fields.insert(
            "thumb_avif_url".into(),
            serde_json::json!("/uploads/m/t.avif"),
        );

        assemble_sizes_object(&mut doc, &upload);

        // The else branch should remove format columns even without URL
        assert!(
            !doc.fields.contains_key("thumb_webp_url"),
            "Orphaned webp column should be removed"
        );
        assert!(
            !doc.fields.contains_key("thumb_avif_url"),
            "Orphaned avif column should be removed"
        );
        assert!(
            !doc.fields.contains_key("sizes"),
            "No sizes object since no URL"
        );
    }

    #[test]
    fn assemble_sizes_partial_dimensions() {
        use crate::core::Document;

        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumb")
                .width(100)
                .height(100)
                .build(),
        ];

        let mut doc = Document::new("id1".into());
        doc.fields
            .insert("thumb_url".into(), serde_json::json!("/uploads/m/t.png"));
        // Only width, no height
        doc.fields
            .insert("thumb_width".into(), serde_json::json!(100));

        assemble_sizes_object(&mut doc, &upload);

        let sizes = doc.fields.get("sizes").expect("sizes");
        let thumb = sizes.get("thumb").expect("thumb");
        assert!(thumb.get("width").is_some());
        assert!(
            thumb.get("height").is_none(),
            "Missing height should not appear"
        );
        // No formats since format_options is default (None)
        assert!(thumb.get("formats").is_none());
    }

    #[test]
    fn inject_upload_metadata_basic() {
        let processed =
            ProcessedUploadBuilder::new("abc_photo.png", "/uploads/media/abc_photo.png")
                .mime_type("image/png")
                .filesize(12345)
                .width(800)
                .height(600)
                .build();
        let mut form_data = HashMap::new();
        inject_upload_metadata(&mut form_data, &processed);

        assert_eq!(form_data.get("filename").unwrap(), "abc_photo.png");
        assert_eq!(form_data.get("mime_type").unwrap(), "image/png");
        assert_eq!(form_data.get("filesize").unwrap(), "12345");
        assert_eq!(form_data.get("width").unwrap(), "800");
        assert_eq!(form_data.get("height").unwrap(), "600");
        assert_eq!(
            form_data.get("url").unwrap(),
            "/uploads/media/abc_photo.png"
        );
    }

    #[test]
    fn inject_upload_metadata_no_dimensions() {
        let processed = ProcessedUploadBuilder::new("doc.pdf", "/uploads/docs/doc.pdf")
            .mime_type("application/pdf")
            .filesize(999)
            .build();
        let mut form_data = HashMap::new();
        inject_upload_metadata(&mut form_data, &processed);

        assert!(!form_data.contains_key("width"));
        assert!(!form_data.contains_key("height"));
        assert_eq!(form_data.get("filename").unwrap(), "doc.pdf");
    }

    #[test]
    fn inject_upload_metadata_with_sizes() {
        let mut formats = HashMap::new();
        formats.insert("webp".into(), FormatResult::new("/uploads/m/t.webp"));
        let mut sizes = HashMap::new();
        sizes.insert(
            "thumb".into(),
            SizeResultBuilder::new("/uploads/m/t.png")
                .width(100)
                .height(100)
                .formats(formats)
                .build(),
        );

        let processed = ProcessedUploadBuilder::new("img.png", "/uploads/m/img.png")
            .mime_type("image/png")
            .filesize(5000)
            .width(800)
            .height(600)
            .sizes(sizes)
            .build();
        let mut form_data = HashMap::new();
        inject_upload_metadata(&mut form_data, &processed);

        assert_eq!(form_data.get("thumb_url").unwrap(), "/uploads/m/t.png");
        assert_eq!(form_data.get("thumb_width").unwrap(), "100");
        assert_eq!(form_data.get("thumb_height").unwrap(), "100");
        assert_eq!(
            form_data.get("thumb_webp_url").unwrap(),
            "/uploads/m/t.webp"
        );
    }

    #[test]
    fn delete_upload_files_removes_existing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let uploads_dir = tmp.path().join("uploads/media");
        std::fs::create_dir_all(&uploads_dir).unwrap();
        let file_path = uploads_dir.join("test.png");
        std::fs::write(&file_path, b"fake image data").unwrap();

        let mut doc_fields = HashMap::new();
        doc_fields.insert("url".into(), serde_json::json!("/uploads/media/test.png"));

        delete_upload_files(tmp.path(), &doc_fields);
        assert!(!file_path.exists(), "File should be deleted");
    }

    #[test]
    fn delete_upload_files_handles_missing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut doc_fields = HashMap::new();
        doc_fields.insert(
            "url".into(),
            serde_json::json!("/uploads/media/nonexistent.png"),
        );

        // Should not panic even if file doesn't exist
        delete_upload_files(tmp.path(), &doc_fields);
    }

    #[test]
    fn delete_upload_files_skips_non_upload_urls() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut doc_fields = HashMap::new();
        doc_fields.insert(
            "url".into(),
            serde_json::json!("https://external.com/image.png"),
        );
        doc_fields.insert(
            "website_url".into(),
            serde_json::json!("https://example.com"),
        );

        // Should not panic and not try to delete external URLs
        delete_upload_files(tmp.path(), &doc_fields);
    }

    #[test]
    fn delete_upload_files_removes_size_and_format_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let uploads_dir = tmp.path().join("uploads/media");
        std::fs::create_dir_all(&uploads_dir).unwrap();

        let orig_path = uploads_dir.join("orig.png");
        let thumb_path = uploads_dir.join("orig_thumb.png");
        let webp_path = uploads_dir.join("orig_thumb.webp");
        std::fs::write(&orig_path, b"orig").unwrap();
        std::fs::write(&thumb_path, b"thumb").unwrap();
        std::fs::write(&webp_path, b"webp").unwrap();

        let mut doc_fields = HashMap::new();
        doc_fields.insert("url".into(), serde_json::json!("/uploads/media/orig.png"));
        doc_fields.insert(
            "thumb_url".into(),
            serde_json::json!("/uploads/media/orig_thumb.png"),
        );
        doc_fields.insert(
            "thumb_webp_url".into(),
            serde_json::json!("/uploads/media/orig_thumb.webp"),
        );

        delete_upload_files(tmp.path(), &doc_fields);
        assert!(!orig_path.exists());
        assert!(!thumb_path.exists());
        assert!(!webp_path.exists());
    }

    #[test]
    fn delete_upload_files_skips_image_url_fields() {
        // Fields containing "image" in the key should be skipped
        let tmp = tempfile::tempdir().expect("tempdir");
        let uploads_dir = tmp.path().join("uploads/media");
        std::fs::create_dir_all(&uploads_dir).unwrap();
        let file_path = uploads_dir.join("keep.png");
        std::fs::write(&file_path, b"keep me").unwrap();

        let mut doc_fields = HashMap::new();
        doc_fields.insert(
            "image_url".into(),
            serde_json::json!("/uploads/media/keep.png"),
        );

        delete_upload_files(tmp.path(), &doc_fields);
        assert!(file_path.exists(), "image_url fields should be skipped");
    }

    #[test]
    fn delete_upload_files_skips_non_string_values() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut doc_fields = HashMap::new();
        doc_fields.insert("url".into(), serde_json::json!(42));
        doc_fields.insert("thumb_url".into(), serde_json::json!(null));

        // Should not panic on non-string values
        delete_upload_files(tmp.path(), &doc_fields);
    }
}
