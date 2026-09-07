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

/// Storage keys of the SERVER-DERIVED url columns (`url`, `{size}_url`,
/// `{size}_{fmt}_url`) present in `doc_fields`, restricted to the authoritative
/// [`CollectionUpload::system_field_names`] set. A USER field that merely ends
/// in `_url` — an external `image_url`, a `source_url`, and so on — is never in
/// that set, so it is never treated as a managed file: a forged value there
/// cannot delete another document's file, and an unrelated external URL is not
/// removed. (This replaces an `ends_with("_url")` string heuristic with a
/// hand-maintained `image_url` exception that missed every other user field.)
#[must_use]
pub fn upload_file_keys(doc_fields: &DocumentFields, upload: &CollectionUpload) -> Vec<String> {
    let system = upload.system_field_names();

    doc_fields
        .as_map()
        .iter()
        .filter(|(key, _)| {
            (key.as_str() == "url" || key.ends_with("_url")) && system.contains(key.as_str())
        })
        .filter_map(|(_, value)| value.as_str())
        .filter_map(|url| key_from_served_url(url).map(str::to_string))
        .collect()
}

/// Delete the given storage keys, logging (not failing) on a per-key error.
/// Callers resolve the keys with [`upload_file_keys`] where the collection's
/// upload config is in scope, so deletion itself needs no schema — which lets
/// the deferred post-commit cleanup queue carry plain keys across collections.
pub fn delete_storage_keys(storage: &dyn StorageBackend, keys: &[String]) {
    for key in keys {
        tracing::debug!("Deleting upload file: {}", key);
        if let Err(e) = storage.delete(key) {
            tracing::warn!("Failed to delete upload key '{}': {}", key, e);
        }
    }
}

/// Delete every server-derived upload file of one document. Convenience for a
/// single-collection caller that has the [`CollectionUpload`] in scope; the
/// cross-collection deferred-cleanup queue instead stores pre-resolved keys
/// (see [`upload_file_keys`]) and drains via [`delete_storage_keys`].
pub fn delete_upload_files(
    storage: &dyn StorageBackend,
    doc_fields: &DocumentFields,
    upload: &CollectionUpload,
) {
    delete_storage_keys(storage, &upload_file_keys(doc_fields, upload));
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

    fn bare_upload() -> CollectionUpload {
        CollectionUpload::new()
    }

    fn upload_with_thumb_webp() -> CollectionUpload {
        let mut u = CollectionUpload::new();
        u.image_sizes = vec![
            ImageSizeBuilder::new("thumb")
                .width(100)
                .height(100)
                .build(),
        ];
        u.format_options.webp = Some(FormatQuality::new(80, false));
        u
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

        delete_upload_files(&storage, &doc_fields, &bare_upload());
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

        // Should not panic even if file doesn't exist.
        delete_upload_files(&storage, &doc_fields, &bare_upload());
    }

    #[test]
    fn delete_upload_files_skips_non_upload_urls() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("url".into(), json!("https://external.com/image.png"));
        doc_fields.insert("website_url".into(), json!("https://example.com"));

        // External URLs resolve to no storage key — nothing to delete, no panic.
        delete_upload_files(&storage, &doc_fields, &bare_upload());
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

        delete_upload_files(&storage, &doc_fields, &upload_with_thumb_webp());
        assert!(!storage.exists("media/orig.png").unwrap());
        assert!(!storage.exists("media/orig_thumb.png").unwrap());
        assert!(!storage.exists("media/orig_thumb.webp").unwrap());
    }

    /// A USER field that ends in `_url` but is NOT a server-derived upload column
    /// (an external `image_url`, a `source_url`, a `hero_image_url` with no
    /// matching size) is never treated as a managed file — its target is NOT
    /// deleted. This is the authoritative-set fix: the old `ends_with("_url")`
    /// heuristic would delete whatever file such a (possibly forged) value
    /// pointed at, enabling cross-document file deletion.
    #[test]
    fn delete_upload_files_leaves_user_url_fields_untouched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        storage.put("media/del.png", b"x", "image/png").unwrap();
        storage
            .put("media/external.png", b"x", "image/png")
            .unwrap();
        storage.put("media/victim.png", b"x", "image/png").unwrap();
        storage.put("media/hero.png", b"x", "image/png").unwrap();

        let mut doc_fields = DocumentFields::new();
        // Server-derived — deleted.
        doc_fields.insert("url".into(), json!("/uploads/media/del.png"));
        // User fields ending in `_url` — not in `system_field_names` → preserved.
        doc_fields.insert("image_url".into(), json!("/uploads/media/external.png"));
        doc_fields.insert("source_url".into(), json!("/uploads/media/victim.png"));
        doc_fields.insert("hero_image_url".into(), json!("/uploads/media/hero.png"));

        delete_upload_files(&storage, &doc_fields, &bare_upload());

        assert!(
            !storage.exists("media/del.png").unwrap(),
            "the server-derived `url` is deleted"
        );
        assert!(
            storage.exists("media/external.png").unwrap(),
            "a user `image_url` is preserved"
        );
        assert!(
            storage.exists("media/victim.png").unwrap(),
            "a forged user `source_url` cannot delete another document's file"
        );
        assert!(
            storage.exists("media/hero.png").unwrap(),
            "a user `hero_image_url` with no matching size is preserved"
        );
    }

    #[test]
    fn upload_file_keys_returns_only_server_derived_columns() {
        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("url".into(), json!("/uploads/media/a.png"));
        doc_fields.insert("thumb_url".into(), json!("/uploads/media/a_thumb.png"));
        doc_fields.insert("source_url".into(), json!("/uploads/media/other.png"));
        // A system column that is not url-bearing is excluded.
        doc_fields.insert("width".into(), json!(100));

        let mut keys = upload_file_keys(&doc_fields, &upload_with_thumb_webp());
        keys.sort();
        assert_eq!(
            keys,
            vec!["media/a.png".to_string(), "media/a_thumb.png".to_string()]
        );
    }

    #[test]
    fn delete_upload_files_skips_non_string_values() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("url".into(), json!(42));
        doc_fields.insert("thumb_url".into(), json!(null));

        // Should not panic on non-string values.
        delete_upload_files(&storage, &doc_fields, &upload_with_thumb_webp());
    }

    #[test]
    fn delete_upload_files_path_traversal_is_harmless() {
        // `key_from_served_url` + the storage backend reject traversal; this just
        // verifies the function handles such a value without panicking.
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);

        let mut doc_fields = DocumentFields::new();
        doc_fields.insert("url".into(), json!("/uploads/../secret.txt"));

        // Should not panic — storage.delete handles non-existent keys gracefully
        delete_upload_files(&storage, &doc_fields, &bare_upload());
    }
}
