use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};

use super::collection_upload::CollectionUpload;
use super::format::FormatResult;
use super::uploaded_file::UploadedFile;
use super::processed_upload::ProcessedUpload;
use super::processed_upload_builder::ProcessedUploadBuilder;
use super::queued_conversion_builder::QueuedConversionBuilder;
use super::size_result_builder::SizeResultBuilder;
use super::validate::{validate_mime_type, sanitize_filename, mime_matches, format_filesize};
use super::resize::{resize_image, save_webp, save_avif};

/// RAII guard that deletes written files if the upload process fails.
/// Call `commit()` on success to prevent cleanup.
struct CleanupGuard {
    files: Vec<PathBuf>,
    committed: bool,
}

impl CleanupGuard {
    fn new() -> Self {
        Self { files: Vec::new(), committed: false }
    }

    fn push(&mut self, path: PathBuf) {
        self.files.push(path);
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if !self.committed {
            for path in &self.files {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// Process an uploaded file: validate, save to disk, generate image sizes + format variants.
///
/// Takes `UploadedFile` by value so this function can be moved into `spawn_blocking`.
pub fn process_upload(
    file: UploadedFile,
    upload_config: &CollectionUpload,
    config_dir: &std::path::Path,
    collection_slug: &str,
    global_max_file_size: u64,
) -> Result<ProcessedUpload> {
    // Validate MIME type against allowlist
    if !validate_mime_type(&file.content_type, &upload_config.mime_types) {
        bail!("File type '{}' is not allowed", file.content_type);
    }

    // Magic-byte MIME verification: check that the file content matches the claimed type.
    // If `infer` can detect the type (images, videos, archives, etc.) it must match the
    // claimed content_type. Files without magic bytes (text, CSS, JS) pass through.
    if let Some(detected) = infer::get(&file.data) {
        let detected_mime = detected.mime_type();
        if !mime_matches(detected_mime, &file.content_type)
            && !mime_matches(&file.content_type, detected_mime)
        {
            bail!(
                "File content does not match claimed type '{}' (detected '{}')",
                file.content_type,
                detected_mime,
            );
        }
    }

    // Validate file size
    let max_size = upload_config.max_file_size.unwrap_or(global_max_file_size);
    let filesize = file.data.len() as u64;
    if filesize > max_size {
        bail!(
            "File size {} exceeds maximum allowed size {}",
            format_filesize(filesize),
            format_filesize(max_size),
        );
    }

    // Generate unique filename
    let id = nanoid::nanoid!(10);
    let sanitized = sanitize_filename(&file.filename);
    let unique_filename = format!("{}_{}", id, sanitized);

    // Create upload directory
    let upload_dir = config_dir.join("uploads").join(collection_slug);
    std::fs::create_dir_all(&upload_dir)
        .with_context(|| format!("Failed to create upload directory: {}", upload_dir.display()))?;

    // Track written files for cleanup on error
    let mut guard = CleanupGuard::new();

    // Save original file
    let original_path = upload_dir.join(&unique_filename);
    std::fs::write(&original_path, &file.data)
        .with_context(|| format!("Failed to write file: {}", original_path.display()))?;
    guard.push(original_path);

    let url = format!("/uploads/{}/{}", collection_slug, unique_filename);

    let is_image = file.content_type.starts_with("image/");

    let mut width = None;
    let mut height = None;
    let mut sizes = HashMap::new();
    let mut queued_conversions = Vec::new();

    if is_image {
        // Load image for processing
        let img = image::load_from_memory(&file.data)
            .with_context(|| "Failed to decode image")?;

        width = Some(img.width());
        height = Some(img.height());

        // Generate format variants for original
        // (We skip original format conversion — only sizes get format variants)

        // Generate image sizes
        for size_def in &upload_config.image_sizes {
            let resized = resize_image(&img, size_def);
            let (stem, ext) = unique_filename.rsplit_once('.')
                .unwrap_or((&unique_filename, "bin"));

            let size_filename = format!("{}_{}.{}", stem, size_def.name, ext);
            let size_path = upload_dir.join(&size_filename);
            resized.save(&size_path)
                .with_context(|| format!("Failed to save resized image: {}", size_path.display()))?;
            guard.push(size_path.clone());

            let size_url = format!("/uploads/{}/{}", collection_slug, size_filename);
            let mut formats = HashMap::new();

            // WebP variant
            if let Some(ref webp_opts) = upload_config.format_options.webp {
                let webp_filename = format!("{}_{}.webp", stem, size_def.name);
                let webp_path = upload_dir.join(&webp_filename);
                let webp_url = format!("/uploads/{}/{}", collection_slug, webp_filename);

                if webp_opts.queue {
                    queued_conversions.push(QueuedConversionBuilder::new(
                        size_path.to_string_lossy(),
                        webp_path.to_string_lossy(),
                    )
                    .format("webp")
                    .quality(webp_opts.quality)
                    .url_column(format!("{}_webp_url", size_def.name))
                    .url_value(webp_url)
                    .build());
                } else {
                    save_webp(&resized, &webp_path, webp_opts.quality)?;
                    guard.push(webp_path);
                    formats.insert("webp".to_string(), FormatResult::new(webp_url));
                }
            }

            // AVIF variant
            if let Some(ref avif_opts) = upload_config.format_options.avif {
                let avif_filename = format!("{}_{}.avif", stem, size_def.name);
                let avif_path = upload_dir.join(&avif_filename);
                let avif_url = format!("/uploads/{}/{}", collection_slug, avif_filename);

                if avif_opts.queue {
                    queued_conversions.push(QueuedConversionBuilder::new(
                        size_path.to_string_lossy(),
                        avif_path.to_string_lossy(),
                    )
                    .format("avif")
                    .quality(avif_opts.quality)
                    .url_column(format!("{}_avif_url", size_def.name))
                    .url_value(avif_url)
                    .build());
                } else {
                    save_avif(&resized, &avif_path, avif_opts.quality)?;
                    guard.push(avif_path);
                    formats.insert("avif".to_string(), FormatResult::new(avif_url));
                }
            }

            sizes.insert(size_def.name.clone(), SizeResultBuilder::new(size_url)
                .width(resized.width())
                .height(resized.height())
                .formats(formats)
                .build());
        }
    }

    let created_files = guard.files.clone();
    guard.commit();
    let mut builder = ProcessedUploadBuilder::new(unique_filename, url)
        .mime_type(file.content_type.clone())
        .filesize(filesize)
        .sizes(sizes)
        .queued_conversions(queued_conversions)
        .created_files(created_files);
    if let Some(w) = width {
        builder = builder.width(w);
    }
    if let Some(h) = height {
        builder = builder.height(h);
    }
    Ok(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::upload::{
        FormatOptions, FormatQuality, ImageFit, ImageSizeBuilder, UploadedFileBuilder,
    };

    /// Create a small test PNG image in memory.
    fn create_test_png(width: u32, height: u32) -> Vec<u8> {
        use image::{ImageBuffer, Rgba, ImageEncoder};
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_fn(width, height, |x, y| {
            Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255])
        });
        let mut buf = Vec::new();
        let encoder = image::codecs::png::PngEncoder::new(&mut buf);
        encoder.write_image(
            img.as_raw(),
            width,
            height,
            image::ExtendedColorType::Rgba8,
        ).expect("encode PNG");
        buf
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
        let result = process_upload(file, &upload_config, tmp.path(), "test", 10_000_000);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not match claimed type"), "Error: {}", err);
    }

    #[test]
    fn magic_byte_verification_allows_matching_type() {
        // PNG magic bytes with correct content_type
        let png_header = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90wS\xde";
        let file = UploadedFileBuilder::new("image.png", "image/png")
            .data(png_header.to_vec())
            .build();
        let mut upload_config = CollectionUpload::default();
        upload_config.mime_types = vec!["image/*".into()];
        let tmp = tempfile::tempdir().unwrap();
        // Won't fully succeed (no valid full PNG) but passes the MIME check
        let result = process_upload(file, &upload_config, tmp.path(), "test", 10_000_000);
        // Should pass MIME validation (might fail later on image processing, that's OK)
        let err_msg = result.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
        assert!(!err_msg.contains("does not match claimed type"), "Unexpected mismatch: {}", err_msg);
    }

    #[test]
    fn magic_byte_verification_passes_text_files() {
        // Plain text has no magic bytes — infer returns None, so it passes through
        let file = UploadedFileBuilder::new("readme.txt", "text/plain")
            .data(b"Hello, world!".to_vec())
            .build();
        let upload_config = CollectionUpload::default();
        let tmp = tempfile::tempdir().unwrap();
        let result = process_upload(file, &upload_config, tmp.path(), "test", 10_000_000);
        let err_msg = result.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
        assert!(!err_msg.contains("does not match claimed type"), "Unexpected mismatch: {}", err_msg);
    }

    #[test]
    fn process_upload_rejects_invalid_mime() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = UploadedFileBuilder::new("test.txt", "text/plain")
            .data(b"hello".to_vec())
            .build();
        let mut config = CollectionUpload::default();
        config.enabled = true;
        config.mime_types = vec!["image/*".into()];
        let result = process_upload(file, &config, tmp.path(), "posts", 50 * 1024 * 1024);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("text/plain"), "Error should mention the rejected MIME type");
    }

    #[test]
    fn process_upload_rejects_oversized_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = UploadedFileBuilder::new("big.bin", "application/octet-stream")
            .data(vec![0u8; 1024]) // 1KB
            .build();
        let mut config = CollectionUpload::default();
        config.enabled = true;
        config.max_file_size = Some(512); // only allow 512 bytes
        let result = process_upload(file, &config, tmp.path(), "posts", 50 * 1024 * 1024);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("exceeds"), "Error should mention size exceeded");
    }

    #[test]
    fn process_upload_uses_global_max_when_no_per_collection_limit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = UploadedFileBuilder::new("big.bin", "application/octet-stream")
            .data(vec![0u8; 1024]) // 1KB
            .build();
        let mut config = CollectionUpload::default();
        config.enabled = true;
        // Global max is 512 bytes
        let result = process_upload(file, &config, tmp.path(), "posts", 512);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exceeds"));
    }

    #[test]
    fn process_upload_non_image_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = UploadedFileBuilder::new("document.pdf", "application/pdf")
            .data(b"%PDF-1.4 test content".to_vec())
            .build();
        let mut config = CollectionUpload::default();
        config.enabled = true;
        let result = process_upload(file, &config, tmp.path(), "docs", 50 * 1024 * 1024)
            .expect("should succeed for non-image");
        assert!(result.url.starts_with("/uploads/docs/"));
        assert!(result.url.ends_with("document.pdf"));
        assert_eq!(result.mime_type, "application/pdf");
        assert_eq!(result.filesize, 21);
        assert!(result.width.is_none());
        assert!(result.height.is_none());
        assert!(result.sizes.is_empty());
        // Verify file was written to disk
        let on_disk = tmp.path().join("uploads/docs").join(&result.filename);
        assert!(on_disk.exists(), "File should be saved to disk");
    }

    #[test]
    fn process_upload_image_no_sizes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let png_data = create_test_png(50, 50);
        let file = UploadedFileBuilder::new("photo.png", "image/png")
            .data(png_data)
            .build();
        let mut config = CollectionUpload::default();
        config.enabled = true;
        let result = process_upload(file, &config, tmp.path(), "media", 50 * 1024 * 1024)
            .expect("should succeed for image");
        assert_eq!(result.mime_type, "image/png");
        assert_eq!(result.width, Some(50));
        assert_eq!(result.height, Some(50));
        assert!(result.sizes.is_empty(), "No image_sizes configured, so no sizes generated");
    }

    #[test]
    fn process_upload_image_with_sizes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let png_data = create_test_png(200, 200);
        let file = UploadedFileBuilder::new("photo.png", "image/png")
            .data(png_data)
            .build();
        let mut config = CollectionUpload::default();
        config.enabled = true;
        config.image_sizes = vec![
            ImageSizeBuilder::new("thumb").width(50).height(50).fit(ImageFit::Cover).build(),
        ];
        let result = process_upload(file, &config, tmp.path(), "media", 50 * 1024 * 1024)
            .expect("should succeed");
        assert_eq!(result.width, Some(200));
        assert_eq!(result.height, Some(200));
        assert!(result.sizes.contains_key("thumb"));
        let thumb = &result.sizes["thumb"];
        assert_eq!(thumb.width, 50);
        assert_eq!(thumb.height, 50);
        assert!(thumb.url.contains("_thumb.png"));
        // Verify the resized file was written
        let thumb_filename = thumb.url.strip_prefix("/uploads/media/").unwrap();
        let thumb_path = tmp.path().join("uploads/media").join(thumb_filename);
        assert!(thumb_path.exists(), "Thumbnail file should be saved");
    }

    #[test]
    fn process_upload_image_with_webp_format() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let png_data = create_test_png(100, 100);
        let file = UploadedFileBuilder::new("photo.png", "image/png")
            .data(png_data)
            .build();
        let mut config = CollectionUpload::default();
        config.enabled = true;
        config.image_sizes = vec![
            ImageSizeBuilder::new("small").width(30).height(30).fit(ImageFit::Cover).build(),
        ];
        config.format_options = FormatOptions {
            webp: Some(FormatQuality::new(80, false)),
            avif: None,
        };
        let result = process_upload(file, &config, tmp.path(), "media", 50 * 1024 * 1024)
            .expect("should succeed");
        let small = &result.sizes["small"];
        assert!(small.formats.contains_key("webp"), "WebP format should be generated");
        let webp = &small.formats["webp"];
        assert!(webp.url.ends_with(".webp"));
    }

    #[test]
    fn process_upload_image_with_avif_format() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let png_data = create_test_png(100, 100);
        let file = UploadedFileBuilder::new("photo.png", "image/png")
            .data(png_data)
            .build();
        let mut config = CollectionUpload::default();
        config.enabled = true;
        config.image_sizes = vec![
            ImageSizeBuilder::new("small").width(30).height(30).fit(ImageFit::Cover).build(),
        ];
        config.format_options = FormatOptions {
            webp: None,
            avif: Some(FormatQuality::new(50, false)),
        };
        let result = process_upload(file, &config, tmp.path(), "media", 50 * 1024 * 1024)
            .expect("should succeed");
        let small = &result.sizes["small"];
        assert!(small.formats.contains_key("avif"), "AVIF format should be generated");
        let avif = &small.formats["avif"];
        assert!(avif.url.ends_with(".avif"));
    }

    #[test]
    fn process_upload_image_with_both_formats() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let png_data = create_test_png(80, 80);
        let file = UploadedFileBuilder::new("photo.png", "image/png")
            .data(png_data)
            .build();
        let mut config = CollectionUpload::default();
        config.enabled = true;
        config.image_sizes = vec![
            ImageSizeBuilder::new("icon").width(20).height(20).fit(ImageFit::Fill).build(),
        ];
        config.format_options = FormatOptions {
            webp: Some(FormatQuality::new(80, false)),
            avif: Some(FormatQuality::new(50, false)),
        };
        let result = process_upload(file, &config, tmp.path(), "media", 50 * 1024 * 1024)
            .expect("should succeed");
        let icon = &result.sizes["icon"];
        assert!(icon.formats.contains_key("webp"));
        assert!(icon.formats.contains_key("avif"));
    }

    #[test]
    fn process_upload_filename_without_extension() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Test with a non-image file that has no extension
        let file = UploadedFileBuilder::new("noext", "application/octet-stream")
            .data(b"binary data".to_vec())
            .build();
        let mut config = CollectionUpload::default();
        config.enabled = true;
        let result = process_upload(file, &config, tmp.path(), "media", 50 * 1024 * 1024)
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
        let png_data = create_test_png(100, 100);
        let file = UploadedFileBuilder::new("test.png", "image/png")
            .data(png_data)
            .build();
        let mut config = CollectionUpload::default();
        config.enabled = true;
        config.image_sizes = vec![
            ImageSizeBuilder::new("thumb").width(30).height(30).fit(ImageFit::Cover).build(),
        ];
        let result = process_upload(file, &config, tmp.path(), "media", 50 * 1024 * 1024)
            .expect("should succeed");
        let thumb = &result.sizes["thumb"];
        assert!(thumb.url.ends_with("_thumb.png"), "Size URL should have .png extension: {}", thumb.url);
    }

    #[test]
    fn process_upload_queue_mode_defers_format_conversion() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let png_data = create_test_png(80, 80);
        let file = UploadedFileBuilder::new("photo.png", "image/png")
            .data(png_data)
            .build();
        let mut config = CollectionUpload::default();
        config.enabled = true;
        config.image_sizes = vec![
            ImageSizeBuilder::new("small").width(30).height(30).fit(ImageFit::Cover).build(),
        ];
        config.format_options = FormatOptions {
            webp: Some(FormatQuality::new(80, true)),
            avif: Some(FormatQuality::new(50, true)),
        };
        let result = process_upload(file, &config, tmp.path(), "media", 50 * 1024 * 1024)
            .expect("should succeed");

        // Sizes should be created but format variants should NOT exist on disk
        let small = &result.sizes["small"];
        assert!(small.formats.is_empty(), "No format variants should be created in queue mode");
        assert!(!small.url.is_empty());

        // Should have queued conversions instead
        assert_eq!(result.queued_conversions.len(), 2);
        let formats: Vec<&str> = result.queued_conversions.iter().map(|q| q.format.as_str()).collect();
        assert!(formats.contains(&"webp"));
        assert!(formats.contains(&"avif"));

        // Verify source paths point to the sized image
        for q in &result.queued_conversions {
            assert!(q.source_path.contains("_small.png"), "Source should be the sized image");
            assert!(!q.url_value.is_empty());
            assert!(!q.url_column.is_empty());
        }
    }

    #[test]
    fn cleanup_guard_removes_files_on_drop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let f1 = tmp.path().join("a.txt");
        let f2 = tmp.path().join("b.txt");
        std::fs::write(&f1, b"a").unwrap();
        std::fs::write(&f2, b"b").unwrap();

        {
            let mut guard = CleanupGuard::new();
            guard.push(f1.clone());
            guard.push(f2.clone());
            // guard drops here without commit
        }

        assert!(!f1.exists(), "f1 should be removed on drop");
        assert!(!f2.exists(), "f2 should be removed on drop");
    }

    #[test]
    fn cleanup_guard_keeps_files_on_commit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let f1 = tmp.path().join("keep.txt");
        std::fs::write(&f1, b"keep").unwrap();

        {
            let mut guard = CleanupGuard::new();
            guard.push(f1.clone());
            guard.commit();
        }

        assert!(f1.exists(), "f1 should remain after commit");
    }
}
