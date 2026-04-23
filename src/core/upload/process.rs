use std::collections::HashMap;

use anyhow::{Context as _, Result};

use super::{
    resize::process_image_sizes,
    validate::{check_image_dimensions, sanitize_filename, validate_upload},
};
use crate::core::upload::{
    CollectionUpload, ProcessedUpload, ProcessedUploadBuilder, SharedStorage, UploadedFile,
};

/// RAII guard that deletes written files if not committed.
/// Returned from [`process_upload`] so callers can commit only after
/// their DB transaction succeeds — preventing orphaned files on rollback.
pub struct CleanupGuard {
    keys: Vec<String>,
    storage: SharedStorage,
    committed: bool,
}

impl std::fmt::Debug for CleanupGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CleanupGuard")
            .field("keys", &self.keys)
            .field("committed", &self.committed)
            .finish()
    }
}

impl CleanupGuard {
    fn new(storage: SharedStorage) -> Self {
        Self {
            keys: Vec::new(),
            storage,
            committed: false,
        }
    }

    pub(super) fn push(&mut self, key: String) {
        self.keys.push(key);
    }

    /// Mark the guard as committed — files will NOT be cleaned up on drop.
    /// Call this after the database transaction has been committed successfully.
    pub fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if !self.committed {
            for key in &self.keys {
                let _ = self.storage.delete(key);
            }
        }
    }
}

/// Save the original file to storage and return `(unique_filename, url)`.
fn save_original(
    file: &UploadedFile,
    storage: &SharedStorage,
    collection_slug: &str,
    guard: &mut CleanupGuard,
) -> Result<(String, String)> {
    let id = nanoid::nanoid!(10);
    let sanitized = sanitize_filename(&file.filename);
    let unique_filename = format!("{}_{}", id, sanitized);

    let original_key = format!("{}/{}", collection_slug, unique_filename);

    storage
        .put(&original_key, &file.data, &file.content_type)
        .with_context(|| format!("Failed to write file: {}", original_key))?;

    guard.push(original_key.clone());

    let url = format!("/uploads/{}", original_key);

    Ok((unique_filename, url))
}

/// Process an uploaded file: validate, save via storage backend, generate image sizes + format variants.
///
/// Returns both the processed upload metadata and a [`CleanupGuard`].
/// The caller **must** call `guard.commit()` after their DB transaction succeeds.
/// If dropped without committing, the guard removes all written files.
///
/// Takes `UploadedFile` by value so this function can be moved into `spawn_blocking`.
pub fn process_upload(
    file: UploadedFile,
    upload_config: &CollectionUpload,
    storage: SharedStorage,
    collection_slug: &str,
    global_max_file_size: u64,
) -> Result<(ProcessedUpload, CleanupGuard)> {
    validate_upload(&file, upload_config, global_max_file_size)?;

    let mut guard = CleanupGuard::new(storage.clone());
    let (unique_filename, url) = save_original(&file, &storage, collection_slug, &mut guard)?;

    let is_image = file.content_type.starts_with("image/");
    let mut width = None;
    let mut height = None;
    let mut sizes = HashMap::new();
    let mut queued_conversions = Vec::new();

    if is_image {
        check_image_dimensions(&file.data)?;

        let img = image::load_from_memory(&file.data).context("Failed to decode image")?;

        width = Some(img.width());
        height = Some(img.height());

        let (s, q) = process_image_sizes(
            &img,
            &unique_filename,
            collection_slug,
            upload_config,
            &storage,
            &mut guard,
        )?;

        sizes = s;
        queued_conversions = q;
    }

    let created_keys = guard.keys.clone();
    let mut builder = ProcessedUploadBuilder::new(unique_filename, url)
        .mime_type(file.content_type.clone())
        .filesize(file.data.len() as u64)
        .sizes(sizes)
        .queued_conversions(queued_conversions)
        .created_files(created_keys);

    if let Some(w) = width {
        builder = builder.width(w);
    }

    if let Some(h) = height {
        builder = builder.height(h);
    }

    Ok((builder.build(), guard))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::core::upload::{
        FormatOptions, FormatQuality, ImageFit, ImageSizeBuilder, UploadedFileBuilder,
        storage::LocalStorage,
    };

    use image::{ImageBuffer, ImageEncoder, Rgba};

    /// Default global max file size used across tests (50 MB).
    const DEFAULT_MAX: u64 = 50 * 1024 * 1024;

    /// Create a small test PNG image in memory.
    fn create_test_png(width: u32, height: u32) -> Vec<u8> {
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_fn(width, height, |x, y| {
            Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        let mut buf = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut buf);
        encoder
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgba8)
            .expect("encode PNG");
        buf
    }

    /// Helper to create a SharedStorage backed by a tempdir.
    fn test_storage(tmp: &tempfile::TempDir) -> SharedStorage {
        Arc::new(LocalStorage::new(tmp.path().join("uploads")))
    }

    #[test]
    fn magic_byte_verification_rejects_mismatched_type() {
        // PNG magic bytes but claimed as text/plain
        let png_header = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90wS\xde";
        let file = UploadedFileBuilder::new("evil.txt", "text/plain")
            .data(png_header.to_vec())
            .build();
        let upload_config = CollectionUpload::default();
        let tmp = tempfile::tempdir().unwrap();
        let storage = test_storage(&tmp);
        let result = process_upload(file, &upload_config, storage.clone(), "test", 10_000_000);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("does not match claimed type"),
            "Error: {}",
            err
        );
    }

    #[test]
    fn magic_byte_verification_allows_matching_type() {
        // PNG magic bytes with correct content_type
        let png_header = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90wS\xde";
        let file = UploadedFileBuilder::new("image.png", "image/png")
            .data(png_header.to_vec())
            .build();
        let upload_config = CollectionUpload {
            mime_types: vec!["image/*".into()],
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let storage = test_storage(&tmp);
        // Won't fully succeed (no valid full PNG) but passes the MIME check
        let result = process_upload(file, &upload_config, storage.clone(), "test", 10_000_000);
        // Should pass MIME validation (might fail later on image processing, that's OK)
        let err_msg = result
            .as_ref()
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            !err_msg.contains("does not match claimed type"),
            "Unexpected mismatch: {}",
            err_msg
        );
    }

    #[test]
    fn magic_byte_verification_passes_text_files() {
        // Plain text has no magic bytes — infer returns None, so it passes through
        let file = UploadedFileBuilder::new("readme.txt", "text/plain")
            .data(b"Hello, world!".to_vec())
            .build();
        let upload_config = CollectionUpload::default();
        let tmp = tempfile::tempdir().unwrap();
        let storage = test_storage(&tmp);
        let result = process_upload(file, &upload_config, storage.clone(), "test", 10_000_000);
        let err_msg = result
            .as_ref()
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            !err_msg.contains("does not match claimed type"),
            "Unexpected mismatch: {}",
            err_msg
        );
    }

    #[test]
    fn mime_verification_is_one_directional() {
        // Regression: the old bidirectional check allowed bypasses where
        // mime_matches(claimed, detected) passed even though
        // mime_matches(detected, claimed) failed.
        //
        // A PNG file claimed as "image/jpeg" must be rejected: the detected
        // MIME "image/png" does not match claimed "image/jpeg".
        let png_data = create_test_png(10, 10);
        let file = UploadedFileBuilder::new("fake.jpg", "image/jpeg")
            .data(png_data)
            .build();
        let config = CollectionUpload {
            enabled: true,
            mime_types: vec!["image/*".into()],
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let storage = test_storage(&tmp);

        let result = process_upload(file, &config, storage.clone(), "test", 10_000_000);
        assert!(
            result.is_err(),
            "Mismatched detected vs claimed MIME should fail"
        );

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("does not match claimed type"),
            "Error should indicate MIME mismatch: {}",
            err_msg
        );
    }

    #[test]
    fn process_upload_rejects_invalid_mime() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let file = UploadedFileBuilder::new("test.txt", "text/plain")
            .data(b"hello".to_vec())
            .build();
        let config = CollectionUpload {
            enabled: true,
            mime_types: vec!["image/*".into()],
            ..Default::default()
        };
        let result = process_upload(file, &config, storage.clone(), "posts", DEFAULT_MAX);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("text/plain"),
            "Error should mention the rejected MIME type"
        );
    }

    #[test]
    fn process_upload_rejects_oversized_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let file = UploadedFileBuilder::new("big.bin", "application/octet-stream")
            .data(vec![0u8; 1024]) // 1KB
            .build();
        let config = CollectionUpload {
            enabled: true,
            max_file_size: Some(512), // only allow 512 bytes
            ..Default::default()
        };
        let result = process_upload(file, &config, storage.clone(), "posts", DEFAULT_MAX);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("exceeds"),
            "Error should mention size exceeded"
        );
    }

    #[test]
    fn process_upload_uses_global_max_when_no_per_collection_limit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let file = UploadedFileBuilder::new("big.bin", "application/octet-stream")
            .data(vec![0u8; 1024]) // 1KB
            .build();
        let config = CollectionUpload {
            enabled: true,
            ..Default::default()
        };
        // Global max is 512 bytes
        let result = process_upload(file, &config, storage.clone(), "posts", 512);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exceeds"));
    }

    #[test]
    fn process_upload_non_image_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let file = UploadedFileBuilder::new("document.pdf", "application/pdf")
            .data(b"%PDF-1.4 test content".to_vec())
            .build();
        let config = CollectionUpload {
            enabled: true,
            ..Default::default()
        };
        let (result, _guard) = process_upload(file, &config, storage.clone(), "docs", DEFAULT_MAX)
            .expect("should succeed for non-image");
        assert!(result.url.starts_with("/uploads/docs/"));
        assert!(result.url.ends_with("document.pdf"));
        assert_eq!(result.mime_type, "application/pdf");
        assert_eq!(result.filesize, 21);
        assert!(result.width.is_none());
        assert!(result.height.is_none());
        assert!(result.sizes.is_empty());

        // Verify file was written via storage
        let key = format!("docs/{}", result.filename);
        assert!(
            storage.exists(&key).unwrap(),
            "File should exist in storage"
        );
    }

    #[test]
    fn process_upload_image_no_sizes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let png_data = create_test_png(50, 50);
        let file = UploadedFileBuilder::new("photo.png", "image/png")
            .data(png_data)
            .build();
        let config = CollectionUpload {
            enabled: true,
            ..Default::default()
        };
        let (result, _guard) = process_upload(file, &config, storage.clone(), "media", DEFAULT_MAX)
            .expect("should succeed for image");
        assert_eq!(result.mime_type, "image/png");
        assert_eq!(result.width, Some(50));
        assert_eq!(result.height, Some(50));
        assert!(
            result.sizes.is_empty(),
            "No image_sizes configured, so no sizes generated"
        );
    }

    #[test]
    fn process_upload_image_with_sizes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let png_data = create_test_png(200, 200);
        let file = UploadedFileBuilder::new("photo.png", "image/png")
            .data(png_data)
            .build();
        let config = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSizeBuilder::new("thumb")
                    .width(50)
                    .height(50)
                    .fit(ImageFit::Cover)
                    .build(),
            ],
            ..Default::default()
        };
        let (result, _guard) = process_upload(file, &config, storage.clone(), "media", DEFAULT_MAX)
            .expect("should succeed");
        assert_eq!(result.width, Some(200));
        assert_eq!(result.height, Some(200));
        assert!(result.sizes.contains_key("thumb"));
        let thumb = &result.sizes["thumb"];
        assert_eq!(thumb.width, 50);
        assert_eq!(thumb.height, 50);
        assert!(thumb.url.contains("_thumb.png"));

        // Verify the resized file was written via storage
        let thumb_key = thumb
            .url
            .strip_prefix("/uploads/")
            .expect("thumb url should start with /uploads/");
        assert!(
            storage.exists(thumb_key).unwrap(),
            "Thumbnail file should exist in storage"
        );
    }

    #[test]
    fn process_upload_image_with_webp_format() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let png_data = create_test_png(100, 100);
        let file = UploadedFileBuilder::new("photo.png", "image/png")
            .data(png_data)
            .build();
        let config = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSizeBuilder::new("small")
                    .width(30)
                    .height(30)
                    .fit(ImageFit::Cover)
                    .build(),
            ],
            format_options: FormatOptions {
                webp: Some(FormatQuality::new(80, false)),
                avif: None,
            },
            ..Default::default()
        };
        let (result, _guard) = process_upload(file, &config, storage.clone(), "media", DEFAULT_MAX)
            .expect("should succeed");
        let small = &result.sizes["small"];
        assert!(
            small.formats.contains_key("webp"),
            "WebP format should be generated"
        );
        let webp = &small.formats["webp"];
        assert!(webp.url.ends_with(".webp"));
    }

    #[test]
    fn process_upload_image_with_avif_format() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let png_data = create_test_png(100, 100);
        let file = UploadedFileBuilder::new("photo.png", "image/png")
            .data(png_data)
            .build();
        let config = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSizeBuilder::new("small")
                    .width(30)
                    .height(30)
                    .fit(ImageFit::Cover)
                    .build(),
            ],
            format_options: FormatOptions {
                webp: None,
                avif: Some(FormatQuality::new(50, false)),
            },
            ..Default::default()
        };
        let (result, _guard) = process_upload(file, &config, storage.clone(), "media", DEFAULT_MAX)
            .expect("should succeed");
        let small = &result.sizes["small"];
        assert!(
            small.formats.contains_key("avif"),
            "AVIF format should be generated"
        );
        let avif = &small.formats["avif"];
        assert!(avif.url.ends_with(".avif"));
    }

    #[test]
    fn process_upload_image_with_both_formats() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let png_data = create_test_png(80, 80);
        let file = UploadedFileBuilder::new("photo.png", "image/png")
            .data(png_data)
            .build();
        let config = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSizeBuilder::new("icon")
                    .width(20)
                    .height(20)
                    .fit(ImageFit::Fill)
                    .build(),
            ],
            format_options: FormatOptions {
                webp: Some(FormatQuality::new(80, false)),
                avif: Some(FormatQuality::new(50, false)),
            },
            ..Default::default()
        };
        let (result, _guard) = process_upload(file, &config, storage.clone(), "media", DEFAULT_MAX)
            .expect("should succeed");
        let icon = &result.sizes["icon"];
        assert!(icon.formats.contains_key("webp"));
        assert!(icon.formats.contains_key("avif"));
    }

    #[test]
    fn process_upload_filename_without_extension() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        // Test with a non-image file that has no extension
        let file = UploadedFileBuilder::new("noext", "application/octet-stream")
            .data(b"binary data".to_vec())
            .build();
        let config = CollectionUpload {
            enabled: true,
            ..Default::default()
        };
        let (result, _guard) = process_upload(file, &config, storage.clone(), "media", DEFAULT_MAX)
            .expect("should succeed even without extension");
        // The filename should have the nanoid prefix and sanitized name
        assert!(result.filename.contains("noext"));
        assert!(result.width.is_none());
        assert!(result.height.is_none());
    }

    #[test]
    fn process_upload_image_with_extension_in_sizes() {
        // Verify that the size URL uses the file extension from the original filename
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let png_data = create_test_png(100, 100);
        let file = UploadedFileBuilder::new("test.png", "image/png")
            .data(png_data)
            .build();
        let config = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSizeBuilder::new("thumb")
                    .width(30)
                    .height(30)
                    .fit(ImageFit::Cover)
                    .build(),
            ],
            ..Default::default()
        };
        let (result, _guard) = process_upload(file, &config, storage.clone(), "media", DEFAULT_MAX)
            .expect("should succeed");
        let thumb = &result.sizes["thumb"];
        assert!(
            thumb.url.ends_with("_thumb.png"),
            "Size URL should have .png extension: {}",
            thumb.url
        );
    }

    #[test]
    fn process_upload_queue_mode_defers_format_conversion() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let png_data = create_test_png(80, 80);
        let file = UploadedFileBuilder::new("photo.png", "image/png")
            .data(png_data)
            .build();
        let config = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSizeBuilder::new("small")
                    .width(30)
                    .height(30)
                    .fit(ImageFit::Cover)
                    .build(),
            ],
            format_options: FormatOptions {
                webp: Some(FormatQuality::new(80, true)),
                avif: Some(FormatQuality::new(50, true)),
            },
            ..Default::default()
        };
        let (result, _guard) = process_upload(file, &config, storage.clone(), "media", DEFAULT_MAX)
            .expect("should succeed");

        // Sizes should be created but format variants should NOT exist
        let small = &result.sizes["small"];
        assert!(
            small.formats.is_empty(),
            "No format variants should be created in queue mode"
        );
        assert!(!small.url.is_empty());

        // Should have queued conversions instead
        assert_eq!(result.queued_conversions.len(), 2);
        let formats: Vec<&str> = result
            .queued_conversions
            .iter()
            .map(|q| q.format.as_str())
            .collect();
        assert!(formats.contains(&"webp"));
        assert!(formats.contains(&"avif"));

        // Verify source paths point to the sized image
        for q in &result.queued_conversions {
            assert!(
                q.source_path.contains("_small.png"),
                "Source should be the sized image"
            );
            assert!(!q.url_value.is_empty());
            assert!(!q.url_column.is_empty());
        }
    }

    /// Regression: queued conversions must record the storage *key*
    /// (`media/foo_small.png`), not an absolute filesystem path. The scheduler
    /// passes these straight to `storage.get()` / `storage.put()`, which reject
    /// absolute paths post-hardening. Using `local_path(...)` here used to
    /// produce a filesystem-absolute `source_path` that the queue runner could
    /// no longer read, failing every queued conversion with
    /// "Source image not found".
    #[test]
    fn process_upload_queue_stores_storage_keys_not_absolute_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let png_data = create_test_png(80, 80);
        let file = UploadedFileBuilder::new("photo.png", "image/png")
            .data(png_data)
            .build();
        let config = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSizeBuilder::new("small")
                    .width(30)
                    .height(30)
                    .fit(ImageFit::Cover)
                    .build(),
            ],
            format_options: FormatOptions {
                webp: Some(FormatQuality::new(80, true)),
                avif: None,
            },
            ..Default::default()
        };
        let (result, _guard) = process_upload(file, &config, storage.clone(), "media", DEFAULT_MAX)
            .expect("should succeed");

        assert!(!result.queued_conversions.is_empty());

        for q in &result.queued_conversions {
            assert!(
                !q.source_path.starts_with('/') && !q.source_path.starts_with('\\'),
                "source_path must be a relative storage key, got: {}",
                q.source_path,
            );
            assert!(
                !q.target_path.starts_with('/') && !q.target_path.starts_with('\\'),
                "target_path must be a relative storage key, got: {}",
                q.target_path,
            );
            assert!(
                q.source_path.starts_with("media/"),
                "source_path must be prefixed with the collection slug, got: {}",
                q.source_path,
            );
            assert!(
                q.target_path.starts_with("media/"),
                "target_path must be prefixed with the collection slug, got: {}",
                q.target_path,
            );
        }
    }

    #[test]
    fn process_upload_guard_cleans_up_on_drop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let file = UploadedFileBuilder::new("test.txt", "application/octet-stream")
            .data(b"test content".to_vec())
            .build();
        let config = CollectionUpload {
            enabled: true,
            ..Default::default()
        };
        let (processed, guard) =
            process_upload(file, &config, storage.clone(), "test", DEFAULT_MAX)
                .expect("should succeed");

        let key = format!("test/{}", processed.filename);
        assert!(
            storage.exists(&key).unwrap(),
            "File should exist after upload"
        );

        drop(guard);
        assert!(
            !storage.exists(&key).unwrap(),
            "File should be cleaned up when guard drops without commit"
        );
    }

    #[test]
    fn cleanup_guard_removes_files_on_drop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);

        storage.put("a.txt", b"a", "text/plain").unwrap();
        storage.put("b.txt", b"b", "text/plain").unwrap();

        {
            let mut guard = CleanupGuard::new(storage.clone());
            guard.push("a.txt".to_string());
            guard.push("b.txt".to_string());
            // guard drops here without commit
        }

        assert!(
            !storage.exists("a.txt").unwrap(),
            "a.txt should be removed on drop"
        );
        assert!(
            !storage.exists("b.txt").unwrap(),
            "b.txt should be removed on drop"
        );
    }

    #[test]
    fn cleanup_guard_keeps_files_on_commit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);

        storage.put("keep.txt", b"keep", "text/plain").unwrap();

        {
            let mut guard = CleanupGuard::new(storage.clone());
            guard.push("keep.txt".to_string());
            guard.commit();
        }

        assert!(
            storage.exists("keep.txt").unwrap(),
            "keep.txt should remain after commit"
        );
    }
}
