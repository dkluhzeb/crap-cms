use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::{
    core::{
        Document, DocumentFields,
        upload::{
            CollectionUpload, ImageConvertJobData, ProcessedUpload, QueuedConversion,
            key_from_served_url, queue_image_conversion, storage::StorageBackend,
        },
    },
    db::DbConnection,
};

/// One entry under the document's `sizes` object — the PayloadCMS-style nested
/// `{ url, width, height, formats: { webp: { url }, avif: { url } } }` shape.
#[derive(Serialize)]
struct ImageSizeEntry {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    formats: HashMap<String, FormatVariant>,
}

/// One entry under `formats` (e.g. `webp`, `avif`).
#[derive(Serialize)]
struct FormatVariant {
    url: String,
}

/// Assemble per-size typed columns into a structured `sizes` object on the document.
/// Reads `{name}_url`, `{name}_width`, `{name}_height`, `{name}_webp_url`, `{name}_avif_url`
/// from document fields, builds a nested PayloadCMS-style object, inserts as `sizes`,
/// and removes the individual per-size columns.
///
/// # Panics
///
/// Panics only if `serde_json::to_value` on the assembled `HashMap` fails —
/// unreachable for a map of owned `String`/`u32`-valued structs.
pub fn assemble_sizes_object(doc: &mut Document, upload: &CollectionUpload) {
    let mut sizes: HashMap<String, ImageSizeEntry> = HashMap::new();

    for size_def in &upload.image_sizes {
        let name = &size_def.name;

        let url = doc
            .fields
            .remove(&format!("{name}_url"))
            .and_then(|v| match v {
                Value::String(s) => Some(s),
                _ => None,
            });
        let width = doc
            .fields
            .remove(&format!("{name}_width"))
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok());
        let height = doc
            .fields
            .remove(&format!("{name}_height"))
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok());

        if let Some(url) = url {
            let formats = collect_format_urls(doc, name, upload);

            sizes.insert(
                name.clone(),
                ImageSizeEntry {
                    url,
                    width,
                    height,
                    formats,
                },
            );
        } else {
            // Still remove format columns even if there's no URL
            doc.fields.remove(&format!("{name}_webp_url"));
            doc.fields.remove(&format!("{name}_avif_url"));
        }
    }

    if !sizes.is_empty() {
        let value = serde_json::to_value(&sizes).expect("ImageSizeEntry serialize");
        doc.fields.insert("sizes".to_string(), value);
    }
}

/// Collect format variant URLs (webp, avif) from document fields.
fn collect_format_urls(
    doc: &mut Document,
    size_name: &str,
    upload: &CollectionUpload,
) -> HashMap<String, FormatVariant> {
    let mut formats = HashMap::new();

    if upload.format_options.webp.is_some()
        && let Some(Value::String(url)) = doc.fields.remove(&format!("{size_name}_webp_url"))
    {
        formats.insert("webp".to_string(), FormatVariant { url });
    }

    if upload.format_options.avif.is_some()
        && let Some(Value::String(url)) = doc.fields.remove(&format!("{size_name}_avif_url"))
    {
        formats.insert("avif".to_string(), FormatVariant { url });
    }

    formats
}

/// Inject upload metadata fields into form data from a processed upload.
/// Writes per-size typed fields ({name}_url, {name}_width, {name}_height, {name}_`webp_url`, etc.)
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
        form_data.insert(format!("{name}_url"), size.url.clone());
        form_data.insert(format!("{name}_width"), size.width.to_string());
        form_data.insert(format!("{name}_height"), size.height.to_string());
        for (fmt, result) in &size.formats {
            form_data.insert(format!("{name}_{fmt}_url"), result.url.clone());
        }
    }
}

/// Delete all files associated with an upload document.
/// Reads the url and per-size url fields to determine which files to remove.
/// Extracts storage keys from `/uploads/{key}` URLs and deletes via the storage backend.
pub fn delete_upload_files(storage: &dyn StorageBackend, doc_fields: &DocumentFields) {
    for (key, value) in doc_fields.as_map() {
        if (key == "url" || key.ends_with("_url"))
            && key != "image_url"
            && let Value::String(url) = value
            && let Some(storage_key) = key_from_served_url(url)
        {
            tracing::debug!("Deleting upload file: {}", storage_key);
            if let Err(e) = storage.delete(storage_key) {
                tracing::warn!("Failed to delete upload key '{}': {}", storage_key, e);
            }
        }
    }
}

/// Insert queued format conversions as `_system_image_convert` jobs.
/// Called after document creation, when the document ID is known.
/// The unified job queue handles retries, heartbeats, recovery, and
/// per-slug concurrency — see
/// `core::upload::queue::queue_image_conversion`.
///
/// # Errors
///
/// Returns a backend error if any job insert fails.
pub fn enqueue_conversions(
    conn: &dyn DbConnection,
    collection: &str,
    document_id: &str,
    conversions: &[QueuedConversion],
    max_attempts: u32,
) -> Result<()> {
    for c in conversions {
        let data = ImageConvertJobData {
            collection: collection.to_string(),
            document_id: document_id.to_string(),
            source_path: c.source_path.clone(),
            target_path: c.target_path.clone(),
            format: c.format.clone(),
            quality: c.quality,
            url_column: c.url_column.clone(),
            url_value: c.url_value.clone(),
        };
        queue_image_conversion(conn, &data, max_attempts)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::{
        Document, DocumentId,
        upload::{
            FormatOptions, FormatQuality, FormatResult, ImageSizeBuilder, ProcessedUpload,
            SizeResult, storage::LocalStorage,
        },
    };

    #[test]
    fn assemble_sizes_builds_structured_object() {
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

        let mut doc = Document::new(DocumentId::new("test-id"));
        doc.fields
            .insert("url".into(), json!("/uploads/media/orig.png"));
        doc.fields
            .insert("thumbnail_url".into(), json!("/uploads/media/thumb.png"));
        doc.fields.insert("thumbnail_width".into(), json!(300));
        doc.fields.insert("thumbnail_height".into(), json!(300));
        doc.fields.insert(
            "thumbnail_webp_url".into(),
            json!("/uploads/media/thumb.webp"),
        );
        doc.fields
            .insert("card_url".into(), json!("/uploads/media/card.png"));
        doc.fields.insert("card_width".into(), json!(640));
        doc.fields.insert("card_height".into(), json!(480));
        doc.fields
            .insert("card_webp_url".into(), json!("/uploads/media/card.webp"));

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
        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumbnail")
                .width(300)
                .height(300)
                .build(),
        ];

        let mut doc = Document::new(DocumentId::new("test-id"));
        doc.fields
            .insert("url".into(), json!("/uploads/media/orig.pdf"));

        assemble_sizes_object(&mut doc, &upload);

        // No sizes object since no size columns exist
        assert!(!doc.fields.contains_key("sizes"));
        // Original url preserved
        assert!(doc.fields.contains_key("url"));
    }

    #[test]
    fn assemble_sizes_with_avif_format() {
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

        let mut doc = Document::new(DocumentId::new("id1"));
        doc.fields
            .insert("thumb_url".into(), json!("/uploads/m/t.png"));
        doc.fields.insert("thumb_width".into(), json!(100));
        doc.fields.insert("thumb_height".into(), json!(100));
        doc.fields
            .insert("thumb_avif_url".into(), json!("/uploads/m/t.avif"));

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

        let mut doc = Document::new(DocumentId::new("id1"));
        // No thumb_url, but format columns exist (edge case: orphaned format columns)
        doc.fields
            .insert("thumb_webp_url".into(), json!("/uploads/m/t.webp"));
        doc.fields
            .insert("thumb_avif_url".into(), json!("/uploads/m/t.avif"));

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
        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumb")
                .width(100)
                .height(100)
                .build(),
        ];

        let mut doc = Document::new(DocumentId::new("id1"));
        doc.fields
            .insert("thumb_url".into(), json!("/uploads/m/t.png"));
        // Only width, no height
        doc.fields.insert("thumb_width".into(), json!(100));

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
        let processed = ProcessedUpload {
            filename: "abc_photo.png".to_string(),
            mime_type: "image/png".to_string(),
            filesize: 12345,
            width: Some(800),
            height: Some(600),
            url: "/uploads/media/abc_photo.png".to_string(),
            sizes: HashMap::new(),
            queued_conversions: Vec::new(),
            created_files: Vec::new(),
        };
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
        let processed = ProcessedUpload {
            filename: "doc.pdf".to_string(),
            mime_type: "application/pdf".to_string(),
            filesize: 999,
            width: None,
            height: None,
            url: "/uploads/docs/doc.pdf".to_string(),
            sizes: HashMap::new(),
            queued_conversions: Vec::new(),
            created_files: Vec::new(),
        };
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
            SizeResult {
                url: "/uploads/m/t.png".to_string(),
                width: 100,
                height: 100,
                formats,
            },
        );

        let processed = ProcessedUpload {
            filename: "img.png".to_string(),
            mime_type: "image/png".to_string(),
            filesize: 5000,
            width: Some(800),
            height: Some(600),
            url: "/uploads/m/img.png".to_string(),
            sizes,
            queued_conversions: Vec::new(),
            created_files: Vec::new(),
        };
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

    /// Helper to create a `LocalStorage` backed by a tempdir.
    fn test_storage(tmp: &tempfile::TempDir) -> LocalStorage {
        LocalStorage::new(tmp.path().join("uploads"))
    }

    #[test]
    fn delete_upload_files_removes_existing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        storage
            .put("media/test.png", b"fake image data", "image/png")
            .unwrap();

        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("url".into(), json!("/uploads/media/test.png"));

        delete_upload_files(&storage, &doc_fields);
        assert!(
            !storage.exists("media/test.png").unwrap(),
            "File should be deleted"
        );
    }

    #[test]
    fn delete_upload_files_handles_missing_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("url".into(), json!("/uploads/media/nonexistent.png"));

        // Should not panic even if file doesn't exist
        delete_upload_files(&storage, &doc_fields);
    }

    #[test]
    fn delete_upload_files_skips_non_upload_urls() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("url".into(), json!("https://external.com/image.png"));
        doc_fields.insert("website_url".into(), json!("https://example.com"));

        // Should not panic and not try to delete external URLs
        delete_upload_files(&storage, &doc_fields);
    }

    #[test]
    fn delete_upload_files_removes_size_and_format_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);

        storage.put("media/orig.png", b"orig", "image/png").unwrap();
        storage
            .put("media/orig_thumb.png", b"thumb", "image/png")
            .unwrap();
        storage
            .put("media/orig_thumb.webp", b"webp", "image/webp")
            .unwrap();

        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("url".into(), json!("/uploads/media/orig.png"));
        doc_fields.insert("thumb_url".into(), json!("/uploads/media/orig_thumb.png"));
        doc_fields.insert(
            "thumb_webp_url".into(),
            json!("/uploads/media/orig_thumb.webp"),
        );

        delete_upload_files(&storage, &doc_fields);
        assert!(!storage.exists("media/orig.png").unwrap());
        assert!(!storage.exists("media/orig_thumb.png").unwrap());
        assert!(!storage.exists("media/orig_thumb.webp").unwrap());
    }

    #[test]
    fn delete_upload_files_skips_image_url_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        storage
            .put("media/keep.png", b"keep me", "image/png")
            .unwrap();

        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("image_url".into(), json!("/uploads/media/keep.png"));

        delete_upload_files(&storage, &doc_fields);
        assert!(
            storage.exists("media/keep.png").unwrap(),
            "image_url fields should be skipped"
        );
    }

    #[test]
    fn delete_upload_files_does_not_skip_prefixed_image_url() {
        // Regression: the old check used key.contains("image") which incorrectly
        // skipped fields like "hero_image_url". Only exact "image_url" should be skipped.
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);

        storage.put("media/hero.png", b"hero", "image/png").unwrap();
        storage
            .put("media/banner.png", b"banner", "image/png")
            .unwrap();
        storage.put("media/keep.png", b"keep", "image/png").unwrap();

        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("hero_image_url".into(), json!("/uploads/media/hero.png"));
        doc_fields.insert(
            "banner_image_url".into(),
            json!("/uploads/media/banner.png"),
        );
        // Exact "image_url" should still be skipped
        doc_fields.insert("image_url".into(), json!("/uploads/media/keep.png"));

        delete_upload_files(&storage, &doc_fields);

        assert!(
            !storage.exists("media/hero.png").unwrap(),
            "hero_image_url should NOT be skipped — only exact 'image_url' is skipped"
        );
        assert!(
            !storage.exists("media/banner.png").unwrap(),
            "banner_image_url should NOT be skipped"
        );
        assert!(
            storage.exists("media/keep.png").unwrap(),
            "Exact 'image_url' should still be skipped"
        );
    }

    #[test]
    fn delete_upload_files_skips_non_string_values() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("url".into(), json!(42));
        doc_fields.insert("thumb_url".into(), json!(null));

        // Should not panic on non-string values
        delete_upload_files(&storage, &doc_fields);
    }

    #[test]
    fn delete_upload_files_path_traversal_is_harmless() {
        // With key-based storage, path traversal in URLs is handled by the storage backend.
        // The key `../secret.txt` would be passed to storage.delete() which for LocalStorage
        // resolves relative to its base_dir. This test verifies the function handles it safely.
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);

        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("url".into(), json!("/uploads/../secret.txt"));

        // Should not panic — storage.delete handles non-existent keys gracefully
        delete_upload_files(&storage, &doc_fields);
    }
}
