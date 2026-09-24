//! Storing an inspected upload: the original, every configured size, and the
//! format variants converted synchronously.

use std::collections::HashMap;

use anyhow::{Context as _, Result};

use super::{exif::apply_exif_orientation, resize::process_image_sizes};
use crate::core::upload::{
    CleanupGuard, CollectionUpload, InspectedUpload, ProcessedUpload, QueuedConversion,
    SharedStorage, SizeResult, served_url,
};

/// Where one upload's files go. Every field is required and it is built at the
/// one call site, so a plain literal stands in for a builder.
pub(super) struct Destination<'a> {
    pub upload: &'a CollectionUpload,
    pub storage: &'a SharedStorage,
    pub collection_slug: &'a str,
}

/// Save the original file to storage and return its served url.
fn save_original(
    inspected: &InspectedUpload<'_>,
    dest: &Destination<'_>,
    guard: &mut CleanupGuard,
) -> Result<String> {
    let key = format!("{}/{}", dest.collection_slug, inspected.columns.filename);

    dest.storage
        .put(&key, &inspected.file.data, &inspected.columns.mime_type)
        .with_context(|| format!("Failed to write file: {key}"))?;

    guard.push(key.clone());

    Ok(served_url(&key))
}

/// The resized sizes of a decodable image, and the format conversions they
/// leave to the queue. Anything else — a non-image, or an `image/*` type this
/// build has no decoder for (SVG, and AVIF unless `image` is built with
/// `avif-native`) — is stored exactly as uploaded, with no sizes.
fn generate_sizes(
    inspected: &InspectedUpload<'_>,
    dest: &Destination<'_>,
    guard: &mut CleanupGuard,
) -> Result<(HashMap<String, SizeResult>, Vec<QueuedConversion>)> {
    if !inspected.is_decodable_image() {
        return Ok((HashMap::new(), Vec::new()));
    }

    let data = &inspected.file.data;
    let img = image::load_from_memory(data).context("Failed to decode image")?;

    // Phones and cameras commonly record images sideways with an EXIF
    // `Orientation` tag instructing the renderer to rotate. The `image` crate
    // ignores the tag, so without this step every portrait photo would ship
    // sideways through the resize and format-conversion pipeline. Re-encoding
    // into PNG/WebP/AVIF below also strips the remaining EXIF metadata (GPS
    // coords, camera identifiers) — a privacy win for any uploads served
    // publicly.
    let img = apply_exif_orientation(data, img);

    process_image_sizes(&img, &inspected.columns.filename, dest, guard)
}

/// Store an inspected upload: save the original, generate image sizes and
/// format variants.
///
/// Returns both the processed upload metadata and a [`CleanupGuard`].
/// The caller **must** call `guard.commit()` after their DB transaction succeeds.
/// If dropped without committing, the guard removes all written files.
///
/// Validation happened in [`inspect_upload`](super::inspect_upload), and the
/// file is stored with exactly the columns it reported.
///
/// # Errors
///
/// Returns an error if the image cannot be decoded or any storage write fails.
pub fn process_upload(
    inspected: InspectedUpload<'_>,
    upload_config: &CollectionUpload,
    storage: &SharedStorage,
    collection_slug: &str,
) -> Result<(ProcessedUpload, CleanupGuard)> {
    let dest = Destination {
        upload: upload_config,
        storage,
        collection_slug,
    };

    let mut guard = CleanupGuard::new(storage.clone());

    let url = save_original(&inspected, &dest, &mut guard)?;
    let (sizes, queued_conversions) = generate_sizes(&inspected, &dest, &mut guard)?;

    let processed = ProcessedUpload {
        file: inspected.columns,
        url,
        sizes,
        queued_conversions,
        created_files: guard.keys().to_vec(),
    };

    Ok((processed, guard))
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use std::sync::Arc;

    use image::{ImageBuffer, ImageEncoder, Rgba};

    use super::*;
    use crate::core::upload::{
        FormatOptions, FormatQuality, ImageFit, ImageSizeBuilder, UploadedFile,
        exif::jpeg_with_orientation, inspect_upload, storage::LocalStorage,
    };

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

    /// Helper to create a `SharedStorage` backed by a tempdir.
    fn test_storage(tmp: &tempfile::TempDir) -> SharedStorage {
        Arc::new(LocalStorage::new(tmp.path().join("uploads")))
    }

    /// Inspect `file`, then store it — the two steps an upload write takes.
    fn process(
        file: &UploadedFile,
        config: &CollectionUpload,
        storage: &SharedStorage,
        slug: &str,
        max: u64,
    ) -> Result<(ProcessedUpload, CleanupGuard)> {
        let inspected = inspect_upload(file, config, max)?;

        process_upload(inspected, config, storage, slug)
    }

    /// The stored file carries exactly the columns its inspection reported —
    /// the ones a check made before storing judged — under the name the
    /// inspection chose.
    #[test]
    fn the_stored_file_carries_the_inspected_columns() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let file = UploadedFile {
            filename: "phone.jpg".to_string(),
            content_type: "image/jpeg".to_string(),
            data: jpeg_with_orientation(8, 4, 6),
        };
        let config = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSizeBuilder::new("thumb")
                    .width(2)
                    .height(4)
                    .fit(ImageFit::Cover)
                    .build(),
            ],
            ..Default::default()
        };

        let inspected = inspect_upload(&file, &config, DEFAULT_MAX).expect("valid");
        let columns = inspected.columns().clone();

        let (result, _guard) =
            process_upload(inspected, &config, &storage, "media").expect("stored");

        assert_eq!(result.file.filename, columns.filename);
        assert_eq!(result.file.mime_type, "image/jpeg");
        assert_eq!(result.file.filesize, columns.filesize);
        assert_eq!((result.file.width, result.file.height), (Some(4), Some(8)));
        assert!(
            storage
                .exists(&format!("media/{}", columns.filename))
                .unwrap()
        );
    }

    #[test]
    fn process_upload_non_image_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let file = UploadedFile {
            filename: "document.pdf".to_string(),
            content_type: "application/pdf".to_string(),
            data: b"%PDF-1.4 test content".to_vec(),
        };
        let config = CollectionUpload {
            enabled: true,
            ..Default::default()
        };
        let (result, _guard) = process(&file, &config, &storage, "docs", DEFAULT_MAX)
            .expect("should succeed for non-image");
        assert!(result.url.starts_with("/uploads/docs/"));
        assert!(result.url.ends_with("document.pdf"));
        assert_eq!(result.file.mime_type, "application/pdf");
        assert_eq!(result.file.filesize, 21);
        assert!(result.file.width.is_none());
        assert!(result.file.height.is_none());
        assert!(result.sizes.is_empty());

        // Verify file was written via storage
        let key = format!("docs/{}", result.file.filename);
        assert!(
            storage.exists(&key).unwrap(),
            "File should exist in storage"
        );
    }

    /// A clean SVG is a valid upload: it has no raster decoder, so it skips
    /// the pixel pipeline and is stored byte-for-byte. Regression — every SVG
    /// used to be rejected with "Failed to detect image format", which made
    /// the XXE-sanitising path (and the documented "only clean SVGs reach
    /// storage" promise) unreachable.
    #[test]
    fn process_upload_stores_a_clean_svg_verbatim() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <rect width="10" height="10" fill="red"/>
        </svg>"#;
        let file = UploadedFile {
            filename: "logo.svg".to_string(),
            content_type: "image/svg+xml".to_string(),
            data: svg.to_vec(),
        };
        let config = CollectionUpload {
            enabled: true,
            mime_types: vec!["image/*".into()],
            image_sizes: vec![
                ImageSizeBuilder::new("thumb")
                    .width(50)
                    .height(50)
                    .fit(ImageFit::Cover)
                    .build(),
            ],
            ..Default::default()
        };

        let (result, _guard) =
            process(&file, &config, &storage, "media", DEFAULT_MAX).expect("should succeed");

        assert_eq!(result.file.mime_type, "image/svg+xml");
        assert!(result.file.width.is_none(), "no decoder, so no dimensions");
        assert!(result.file.height.is_none());
        assert!(result.sizes.is_empty(), "an SVG is not resized");
        assert!(result.queued_conversions.is_empty());

        let key = format!("media/{}", result.file.filename);
        assert_eq!(
            storage.get(&key).expect("stored SVG"),
            svg.to_vec(),
            "the SVG must be stored exactly as uploaded"
        );
    }

    #[test]
    fn process_upload_image_no_sizes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let png_data = create_test_png(50, 50);
        let file = UploadedFile {
            filename: "photo.png".to_string(),
            content_type: "image/png".to_string(),
            data: png_data,
        };
        let config = CollectionUpload {
            enabled: true,
            ..Default::default()
        };
        let (result, _guard) = process(&file, &config, &storage, "media", DEFAULT_MAX)
            .expect("should succeed for image");
        assert_eq!(result.file.mime_type, "image/png");
        assert_eq!(result.file.width, Some(50));
        assert_eq!(result.file.height, Some(50));
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
        let file = UploadedFile {
            filename: "photo.png".to_string(),
            content_type: "image/png".to_string(),
            data: png_data,
        };
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
        let (result, _guard) =
            process(&file, &config, &storage, "media", DEFAULT_MAX).expect("should succeed");
        assert_eq!(result.file.width, Some(200));
        assert_eq!(result.file.height, Some(200));
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
        let file = UploadedFile {
            filename: "photo.png".to_string(),
            content_type: "image/png".to_string(),
            data: png_data,
        };
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
        let (result, _guard) =
            process(&file, &config, &storage, "media", DEFAULT_MAX).expect("should succeed");
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
        let file = UploadedFile {
            filename: "photo.png".to_string(),
            content_type: "image/png".to_string(),
            data: png_data,
        };
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
        let (result, _guard) =
            process(&file, &config, &storage, "media", DEFAULT_MAX).expect("should succeed");
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
        let file = UploadedFile {
            filename: "photo.png".to_string(),
            content_type: "image/png".to_string(),
            data: png_data,
        };
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
        let (result, _guard) =
            process(&file, &config, &storage, "media", DEFAULT_MAX).expect("should succeed");
        let icon = &result.sizes["icon"];
        assert!(icon.formats.contains_key("webp"));
        assert!(icon.formats.contains_key("avif"));
    }

    #[test]
    fn process_upload_filename_without_extension() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        // Test with a non-image file that has no extension
        let file = UploadedFile {
            filename: "noext".to_string(),
            content_type: "application/octet-stream".to_string(),
            data: b"binary data".to_vec(),
        };
        let config = CollectionUpload {
            enabled: true,
            ..Default::default()
        };
        let (result, _guard) = process(&file, &config, &storage, "media", DEFAULT_MAX)
            .expect("should succeed even without extension");
        // The filename should have the nanoid prefix and sanitized name
        assert!(result.file.filename.contains("noext"));
        assert!(result.file.width.is_none());
        assert!(result.file.height.is_none());
    }

    #[test]
    fn process_upload_image_with_extension_in_sizes() {
        // Verify that the size URL uses the file extension from the original filename
        let tmp = tempfile::tempdir().expect("tempdir");
        let storage = test_storage(&tmp);
        let png_data = create_test_png(100, 100);
        let file = UploadedFile {
            filename: "test.png".to_string(),
            content_type: "image/png".to_string(),
            data: png_data,
        };
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
        let (result, _guard) =
            process(&file, &config, &storage, "media", DEFAULT_MAX).expect("should succeed");
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
        let file = UploadedFile {
            filename: "photo.png".to_string(),
            content_type: "image/png".to_string(),
            data: png_data,
        };
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
        let (result, _guard) =
            process(&file, &config, &storage, "media", DEFAULT_MAX).expect("should succeed");

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
        let file = UploadedFile {
            filename: "photo.png".to_string(),
            content_type: "image/png".to_string(),
            data: png_data,
        };
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
        let (result, _guard) =
            process(&file, &config, &storage, "media", DEFAULT_MAX).expect("should succeed");

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
        let file = UploadedFile {
            filename: "test.txt".to_string(),
            content_type: "application/octet-stream".to_string(),
            data: b"test content".to_vec(),
        };
        let config = CollectionUpload {
            enabled: true,
            ..Default::default()
        };
        let (processed, guard) =
            process(&file, &config, &storage, "test", DEFAULT_MAX).expect("should succeed");

        let key = format!("test/{}", processed.file.filename);
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
}
