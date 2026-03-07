//! Upload handling: file validation, image resizing, and format conversion (WebP/AVIF).

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};

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
use serde::{Deserialize, Serialize};

/// Per-collection upload configuration (MIME filtering, image sizes, format options).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CollectionUpload {
    pub enabled: bool,
    #[serde(default)]
    pub mime_types: Vec<String>,
    #[serde(default)]
    pub max_file_size: Option<u64>,
    #[serde(default)]
    pub image_sizes: Vec<ImageSize>,
    #[serde(default)]
    pub admin_thumbnail: Option<String>,
    #[serde(default)]
    pub format_options: FormatOptions,
}

/// A named image resize target (e.g. "thumbnail" at 200x200).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSize {
    pub name: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub fit: ImageFit,
}

/// Optional format conversion settings (WebP and/or AVIF with quality).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FormatOptions {
    #[serde(default)]
    pub webp: Option<FormatQuality>,
    #[serde(default)]
    pub avif: Option<FormatQuality>,
}

/// Quality and processing settings for a converted image format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatQuality {
    pub quality: u8,
    /// When true, this format's conversion is deferred to the background image processing
    /// queue instead of happening synchronously during upload. Default: false.
    #[serde(default)]
    pub queue: bool,
}

/// How an image is resized to fit the target dimensions.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ImageFit {
    #[default]
    Cover,
    Contain,
    Inside,
    Fill,
}

/// Raw uploaded file before processing.
pub struct UploadedFile {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

/// A deferred format conversion to be inserted into the image processing queue.
#[derive(Debug, Clone)]
pub struct QueuedConversion {
    pub source_path: String,
    pub target_path: String,
    pub format: String,
    pub quality: u8,
    pub url_column: String,
    pub url_value: String,
}

/// Result of processing an upload (original + generated sizes/formats).
#[derive(Debug)]
pub struct ProcessedUpload {
    pub filename: String,
    pub mime_type: String,
    pub filesize: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub url: String,
    pub sizes: HashMap<String, SizeResult>,
    /// Format conversions deferred to the background queue (when per-format `queue = true`).
    pub queued_conversions: Vec<QueuedConversion>,
}

/// Output metadata for one generated image size.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizeResult {
    pub url: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub formats: HashMap<String, FormatResult>,
}

/// Output metadata for a single converted format variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatResult {
    pub url: String,
}

/// Check if a content type matches a MIME glob pattern.
/// Supports patterns like "image/*", "application/pdf", etc.
fn mime_matches(content_type: &str, pattern: &str) -> bool {
    if pattern == "*" || pattern == "*/*" {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/*") {
        content_type.starts_with(prefix) && content_type.as_bytes().get(prefix.len()) == Some(&b'/')
    } else {
        content_type == pattern
    }
}

/// Validate MIME type against an allowlist of patterns.
/// Empty allowlist means any MIME type is accepted.
fn validate_mime_type(content_type: &str, allowed: &[String]) -> bool {
    if allowed.is_empty() {
        return true;
    }
    allowed.iter().any(|pattern| mime_matches(content_type, pattern))
}

/// Sanitize a filename: lowercase, replace non-alphanumeric with hyphens, collapse.
fn sanitize_filename(name: &str) -> String {
    let name = name.to_lowercase();
    // Split extension from stem
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s, Some(e)),
        None => (name.as_str(), None),
    };
    let clean_stem: String = stem.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let clean_stem: String = clean_stem.split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    match ext {
        Some(e) => format!("{}.{}", clean_stem, e),
        None => clean_stem,
    }
}

/// Process an uploaded file: validate, save to disk, generate image sizes + format variants.
pub fn process_upload(
    file: &UploadedFile,
    upload_config: &CollectionUpload,
    config_dir: &Path,
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
                    queued_conversions.push(QueuedConversion {
                        source_path: size_path.to_string_lossy().to_string(),
                        target_path: webp_path.to_string_lossy().to_string(),
                        format: "webp".to_string(),
                        quality: webp_opts.quality,
                        url_column: format!("{}_webp_url", size_def.name),
                        url_value: webp_url,
                    });
                } else {
                    save_webp(&resized, &webp_path, webp_opts.quality)?;
                    guard.push(webp_path);
                    formats.insert("webp".to_string(), FormatResult { url: webp_url });
                }
            }

            // AVIF variant
            if let Some(ref avif_opts) = upload_config.format_options.avif {
                let avif_filename = format!("{}_{}.avif", stem, size_def.name);
                let avif_path = upload_dir.join(&avif_filename);
                let avif_url = format!("/uploads/{}/{}", collection_slug, avif_filename);

                if avif_opts.queue {
                    queued_conversions.push(QueuedConversion {
                        source_path: size_path.to_string_lossy().to_string(),
                        target_path: avif_path.to_string_lossy().to_string(),
                        format: "avif".to_string(),
                        quality: avif_opts.quality,
                        url_column: format!("{}_avif_url", size_def.name),
                        url_value: avif_url,
                    });
                } else {
                    save_avif(&resized, &avif_path, avif_opts.quality)?;
                    guard.push(avif_path);
                    formats.insert("avif".to_string(), FormatResult { url: avif_url });
                }
            }

            sizes.insert(size_def.name.clone(), SizeResult {
                url: size_url,
                width: resized.width(),
                height: resized.height(),
                formats,
            });
        }
    }

    guard.commit();
    Ok(ProcessedUpload {
        filename: unique_filename,
        mime_type: file.content_type.clone(),
        filesize,
        width,
        height,
        url,
        sizes,
        queued_conversions,
    })
}

/// Resize an image according to the given size definition and fit mode.
fn resize_image(img: &image::DynamicImage, size: &ImageSize) -> image::DynamicImage {
    let filter = image::imageops::FilterType::CatmullRom;
    match size.fit {
        ImageFit::Cover => {
            // Resize to fill, then center crop
            let src_ratio = img.width() as f64 / img.height() as f64;
            let dst_ratio = size.width as f64 / size.height as f64;

            let (resize_w, resize_h) = if src_ratio > dst_ratio {
                // Source is wider — fit height, crop width
                let h = size.height;
                let w = (img.width() as f64 * (size.height as f64 / img.height() as f64)) as u32;
                (w.max(1), h)
            } else {
                // Source is taller — fit width, crop height
                let w = size.width;
                let h = (img.height() as f64 * (size.width as f64 / img.width() as f64)) as u32;
                (w, h.max(1))
            };

            let resized = img.resize_exact(resize_w, resize_h, filter);
            let x = (resized.width().saturating_sub(size.width)) / 2;
            let y = (resized.height().saturating_sub(size.height)) / 2;
            resized.crop_imm(x, y, size.width.min(resized.width()), size.height.min(resized.height()))
        }
        ImageFit::Contain | ImageFit::Inside => {
            // Resize to fit within bounds, preserving aspect ratio
            img.resize(size.width, size.height, filter)
        }
        ImageFit::Fill => {
            // Stretch to exact dimensions
            img.resize_exact(size.width, size.height, filter)
        }
    }
}

/// Save image as lossy WebP with given quality (via libwebp).
fn save_webp(img: &image::DynamicImage, path: &Path, quality: u8) -> Result<()> {
    let rgba = img.to_rgba8();
    let encoder = webp::Encoder::from_rgba(&rgba, img.width(), img.height());
    let mem = encoder.encode(quality as f32);
    std::fs::write(path, &*mem)
        .with_context(|| format!("Failed to write WebP: {}", path.display()))?;
    Ok(())
}

/// Save image as AVIF with given quality.
fn save_avif(img: &image::DynamicImage, path: &Path, quality: u8) -> Result<()> {
    use image::ImageEncoder;
    let rgba = img.to_rgba8();
    let mut buf = Cursor::new(Vec::new());
    let encoder = image::codecs::avif::AvifEncoder::new_with_speed_quality(&mut buf, 8, quality);
    encoder.write_image(
        rgba.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgba8,
    ).with_context(|| "Failed to encode AVIF")?;
    std::fs::write(path, buf.into_inner())
        .with_context(|| format!("Failed to write AVIF: {}", path.display()))?;
    Ok(())
}

/// Assemble per-size typed columns into a structured `sizes` object on the document.
/// Reads `{name}_url`, `{name}_width`, `{name}_height`, `{name}_webp_url`, `{name}_avif_url`
/// from document fields, builds a nested PayloadCMS-style object, inserts as `sizes`,
/// and removes the individual per-size columns.
pub fn assemble_sizes_object(
    doc: &mut crate::core::Document,
    upload: &CollectionUpload,
) {
    let mut sizes = serde_json::Map::new();

    for size_def in &upload.image_sizes {
        let name = &size_def.name;

        let url = doc.fields.remove(&format!("{}_url", name))
            .and_then(|v| match v { serde_json::Value::String(s) => Some(s), _ => None });
        let width = doc.fields.remove(&format!("{}_width", name))
            .and_then(|v| v.as_f64()).map(|v| v as u32);
        let height = doc.fields.remove(&format!("{}_height", name))
            .and_then(|v| v.as_f64()).map(|v| v as u32);

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

            if upload.format_options.webp.is_some() {
                if let Some(serde_json::Value::String(webp_url)) =
                    doc.fields.remove(&format!("{}_webp_url", name))
                {
                    let mut fmt = serde_json::Map::new();
                    fmt.insert("url".to_string(), serde_json::Value::String(webp_url));
                    formats.insert("webp".to_string(), serde_json::Value::Object(fmt));
                }
            }

            if upload.format_options.avif.is_some() {
                if let Some(serde_json::Value::String(avif_url)) =
                    doc.fields.remove(&format!("{}_avif_url", name))
                {
                    let mut fmt = serde_json::Map::new();
                    fmt.insert("url".to_string(), serde_json::Value::String(avif_url));
                    formats.insert("avif".to_string(), serde_json::Value::Object(fmt));
                }
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
        doc.fields.insert("sizes".to_string(), serde_json::Value::Object(sizes));
    }
}

/// Inject upload metadata fields into form data from a processed upload.
/// Writes per-size typed fields ({name}_url, {name}_width, {name}_height, {name}_webp_url, etc.)
pub fn inject_upload_metadata(form_data: &mut HashMap<String, String>, processed: &ProcessedUpload) {
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
pub fn delete_upload_files(
    config_dir: &Path,
    doc_fields: &HashMap<String, serde_json::Value>,
) {
    // Collect all URL fields that point to upload files
    // These are: url, {size}_url, {size}_webp_url, {size}_avif_url
    for (key, value) in doc_fields {
        if (key == "url" || key.ends_with("_url")) && !key.contains("image") {
            if let serde_json::Value::String(url) = value {
                if url.starts_with("/uploads/") {
                    let rel_path = url.strip_prefix('/').unwrap_or(url);
                    let file_path = config_dir.join(rel_path);
                    if file_path.exists() {
                        if let Err(e) = std::fs::remove_file(&file_path) {
                            tracing::warn!("Failed to delete file {}: {}", file_path.display(), e);
                        }
                    }
                }
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
    use crate::db::query::images::insert_image_queue_entry;
    for c in conversions {
        insert_image_queue_entry(
            conn, collection, document_id,
            &c.source_path, &c.target_path, &c.format, c.quality,
            &c.url_column, &c.url_value,
        )?;
    }
    Ok(())
}

/// Process a single image queue entry: read source, convert to target format, save to disk.
/// Returns Ok(()) on success, Err on failure.
pub fn process_image_entry(
    source_path: &str,
    target_path: &str,
    format: &str,
    quality: u8,
) -> anyhow::Result<()> {
    let source = std::path::Path::new(source_path);
    if !source.exists() {
        anyhow::bail!("Source image not found: {}", source_path);
    }

    let img = image::open(source)
        .with_context(|| format!("Failed to decode image: {}", source_path))?;

    let target = std::path::Path::new(target_path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }

    match format {
        "webp" => save_webp(&img, target, quality)?,
        "avif" => save_avif(&img, target, quality)?,
        _ => anyhow::bail!("Unsupported format: {}", format),
    }

    Ok(())
}

/// Format a file size in human-readable form.
pub fn format_filesize(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_matches_wildcard() {
        assert!(mime_matches("image/png", "image/*"));
        assert!(mime_matches("image/jpeg", "image/*"));
        assert!(!mime_matches("application/pdf", "image/*"));
    }

    #[test]
    fn mime_matches_exact() {
        assert!(mime_matches("application/pdf", "application/pdf"));
        assert!(!mime_matches("application/json", "application/pdf"));
    }

    #[test]
    fn mime_matches_any() {
        assert!(mime_matches("anything/here", "*/*"));
        assert!(mime_matches("text/plain", "*"));
    }

    #[test]
    fn validate_mime_empty_allows_all() {
        assert!(validate_mime_type("anything/here", &[]));
    }

    #[test]
    fn validate_mime_with_patterns() {
        let patterns = vec!["image/*".to_string(), "application/pdf".to_string()];
        assert!(validate_mime_type("image/png", &patterns));
        assert!(validate_mime_type("application/pdf", &patterns));
        assert!(!validate_mime_type("text/plain", &patterns));
    }

    #[test]
    fn magic_byte_verification_rejects_mismatched_type() {
        // PNG magic bytes but claimed as text/plain
        let png_header = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90wS\xde";
        let file = UploadedFile {
            filename: "evil.txt".into(),
            content_type: "text/plain".into(),
            data: png_header.to_vec(),
        };
        let upload_config = CollectionUpload {
            mime_types: vec![], // allow all
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let result = process_upload(&file, &upload_config, tmp.path(), "test", 10_000_000);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not match claimed type"), "Error: {}", err);
    }

    #[test]
    fn magic_byte_verification_allows_matching_type() {
        // PNG magic bytes with correct content_type
        let png_header = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x02\x00\x00\x00\x90wS\xde";
        let file = UploadedFile {
            filename: "image.png".into(),
            content_type: "image/png".into(),
            data: png_header.to_vec(),
        };
        let upload_config = CollectionUpload {
            mime_types: vec!["image/*".into()],
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        // Won't fully succeed (no valid full PNG) but passes the MIME check
        let result = process_upload(&file, &upload_config, tmp.path(), "test", 10_000_000);
        // Should pass MIME validation (might fail later on image processing, that's OK)
        let err_msg = result.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
        assert!(!err_msg.contains("does not match claimed type"), "Unexpected mismatch: {}", err_msg);
    }

    #[test]
    fn magic_byte_verification_passes_text_files() {
        // Plain text has no magic bytes — infer returns None, so it passes through
        let file = UploadedFile {
            filename: "readme.txt".into(),
            content_type: "text/plain".into(),
            data: b"Hello, world!".to_vec(),
        };
        let upload_config = CollectionUpload {
            mime_types: vec![],
            ..Default::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let result = process_upload(&file, &upload_config, tmp.path(), "test", 10_000_000);
        let err_msg = result.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
        assert!(!err_msg.contains("does not match claimed type"), "Unexpected mismatch: {}", err_msg);
    }

    #[test]
    fn sanitize_filename_basic() {
        assert_eq!(sanitize_filename("Hello World.png"), "hello-world.png");
        assert_eq!(sanitize_filename("file (1).jpg"), "file-1.jpg");
        assert_eq!(sanitize_filename("PHOTO.JPEG"), "photo.jpeg");
    }

    #[test]
    fn format_filesize_units() {
        assert_eq!(format_filesize(500), "500 B");
        assert_eq!(format_filesize(1536), "1.5 KB");
        assert_eq!(format_filesize(1048576), "1.0 MB");
    }

    #[test]
    fn assemble_sizes_builds_structured_object() {
        use crate::core::Document;

        let upload = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSize { name: "thumbnail".into(), width: 300, height: 300, fit: ImageFit::Cover },
                ImageSize { name: "card".into(), width: 640, height: 480, fit: ImageFit::Cover },
            ],
            format_options: FormatOptions {
                webp: Some(FormatQuality { quality: 80, queue: false }),
                avif: None,
            },
            ..Default::default()
        };

        let mut doc = Document::new("test-id".into());
        doc.fields.insert("url".into(), serde_json::json!("/uploads/media/orig.png"));
        doc.fields.insert("thumbnail_url".into(), serde_json::json!("/uploads/media/thumb.png"));
        doc.fields.insert("thumbnail_width".into(), serde_json::json!(300));
        doc.fields.insert("thumbnail_height".into(), serde_json::json!(300));
        doc.fields.insert("thumbnail_webp_url".into(), serde_json::json!("/uploads/media/thumb.webp"));
        doc.fields.insert("card_url".into(), serde_json::json!("/uploads/media/card.png"));
        doc.fields.insert("card_width".into(), serde_json::json!(640));
        doc.fields.insert("card_height".into(), serde_json::json!(480));
        doc.fields.insert("card_webp_url".into(), serde_json::json!("/uploads/media/card.webp"));

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
        assert_eq!(thumb.get("url").unwrap().as_str().unwrap(), "/uploads/media/thumb.png");
        assert_eq!(thumb.get("width").unwrap().as_u64().unwrap(), 300);
        assert_eq!(thumb.get("height").unwrap().as_u64().unwrap(), 300);
        let thumb_formats = thumb.get("formats").expect("formats");
        assert_eq!(
            thumb_formats.get("webp").unwrap().get("url").unwrap().as_str().unwrap(),
            "/uploads/media/thumb.webp"
        );

        let card = sizes.get("card").expect("card size");
        assert_eq!(card.get("url").unwrap().as_str().unwrap(), "/uploads/media/card.png");
        assert_eq!(card.get("width").unwrap().as_u64().unwrap(), 640);
    }

    #[test]
    fn assemble_sizes_empty_when_no_size_columns() {
        use crate::core::Document;

        let upload = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSize { name: "thumbnail".into(), width: 300, height: 300, fit: ImageFit::Cover },
            ],
            format_options: FormatOptions::default(),
            ..Default::default()
        };

        let mut doc = Document::new("test-id".into());
        doc.fields.insert("url".into(), serde_json::json!("/uploads/media/orig.pdf"));

        assemble_sizes_object(&mut doc, &upload);

        // No sizes object since no size columns exist
        assert!(!doc.fields.contains_key("sizes"));
        // Original url preserved
        assert!(doc.fields.contains_key("url"));
    }

    // --- Additional coverage tests ---

    #[test]
    fn sanitize_filename_no_extension() {
        assert_eq!(sanitize_filename("README"), "readme");
    }

    #[test]
    fn sanitize_filename_multiple_dots() {
        assert_eq!(sanitize_filename("archive.tar.gz"), "archive-tar.gz");
    }

    #[test]
    fn sanitize_filename_special_chars() {
        assert_eq!(sanitize_filename("my file@#$.png"), "my-file.png");
    }

    #[test]
    fn sanitize_filename_underscores_preserved() {
        assert_eq!(sanitize_filename("my_file_name.jpg"), "my_file_name.jpg");
    }

    #[test]
    fn sanitize_filename_consecutive_hyphens_collapsed() {
        assert_eq!(sanitize_filename("a---b.png"), "a-b.png");
    }

    #[test]
    fn sanitize_filename_leading_trailing_special() {
        // Leading special chars become hyphens that get filtered as empty segments
        assert_eq!(sanitize_filename("---file---.png"), "file.png");
    }

    #[test]
    fn format_filesize_gb() {
        // 2 GB
        assert_eq!(format_filesize(2 * 1024 * 1024 * 1024), "2.0 GB");
    }

    #[test]
    fn format_filesize_zero() {
        assert_eq!(format_filesize(0), "0 B");
    }

    #[test]
    fn format_filesize_exact_boundary_kb() {
        assert_eq!(format_filesize(1024), "1.0 KB");
    }

    #[test]
    fn format_filesize_exact_boundary_mb() {
        assert_eq!(format_filesize(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn format_filesize_exact_boundary_gb() {
        assert_eq!(format_filesize(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn mime_matches_partial_type_no_slash() {
        // "image" without "/*" should not match "image/png" (exact match only)
        assert!(!mime_matches("image/png", "image"));
    }

    #[test]
    fn mime_matches_wildcard_does_not_match_without_slash() {
        // "image/*" should not match "imageextra/png" — must have "/" after prefix
        assert!(!mime_matches("imageextra/png", "image/*"));
    }

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
    fn resize_image_cover_wider_source() {
        // Source is wider than target aspect ratio (landscape → square crop)
        let img = image::DynamicImage::ImageRgba8(
            image::ImageBuffer::from_fn(400, 200, |_, _| image::Rgba([0, 0, 0, 255]))
        );
        let size = ImageSize {
            name: "thumb".into(),
            width: 100,
            height: 100,
            fit: ImageFit::Cover,
        };
        let result = resize_image(&img, &size);
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 100);
    }

    #[test]
    fn resize_image_cover_taller_source() {
        // Source is taller than target aspect ratio (portrait → square crop)
        let img = image::DynamicImage::ImageRgba8(
            image::ImageBuffer::from_fn(200, 400, |_, _| image::Rgba([0, 0, 0, 255]))
        );
        let size = ImageSize {
            name: "thumb".into(),
            width: 100,
            height: 100,
            fit: ImageFit::Cover,
        };
        let result = resize_image(&img, &size);
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 100);
    }

    #[test]
    fn resize_image_contain() {
        // Contain: fits within bounds, preserving aspect ratio
        let img = image::DynamicImage::ImageRgba8(
            image::ImageBuffer::from_fn(400, 200, |_, _| image::Rgba([0, 0, 0, 255]))
        );
        let size = ImageSize {
            name: "card".into(),
            width: 100,
            height: 100,
            fit: ImageFit::Contain,
        };
        let result = resize_image(&img, &size);
        // Should fit within 100x100 preserving 2:1 aspect → 100x50
        assert!(result.width() <= 100);
        assert!(result.height() <= 100);
        // The wider dimension should hit the limit
        assert_eq!(result.width(), 100);
    }

    #[test]
    fn resize_image_inside() {
        // Inside: same as contain (fits within bounds)
        let img = image::DynamicImage::ImageRgba8(
            image::ImageBuffer::from_fn(200, 400, |_, _| image::Rgba([0, 0, 0, 255]))
        );
        let size = ImageSize {
            name: "card".into(),
            width: 100,
            height: 100,
            fit: ImageFit::Inside,
        };
        let result = resize_image(&img, &size);
        assert!(result.width() <= 100);
        assert!(result.height() <= 100);
    }

    #[test]
    fn resize_image_fill() {
        // Fill: stretch to exact dimensions, ignoring aspect ratio
        let img = image::DynamicImage::ImageRgba8(
            image::ImageBuffer::from_fn(400, 200, |_, _| image::Rgba([0, 0, 0, 255]))
        );
        let size = ImageSize {
            name: "banner".into(),
            width: 150,
            height: 75,
            fit: ImageFit::Fill,
        };
        let result = resize_image(&img, &size);
        assert_eq!(result.width(), 150);
        assert_eq!(result.height(), 75);
    }

    #[test]
    fn save_webp_writes_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let img = image::DynamicImage::ImageRgba8(
            image::ImageBuffer::from_fn(10, 10, |_, _| image::Rgba([255, 0, 0, 255]))
        );
        let path = tmp.path().join("test.webp");
        save_webp(&img, &path, 80).expect("save_webp should succeed");
        assert!(path.exists(), "WebP file should be created");
        assert!(std::fs::metadata(&path).unwrap().len() > 0, "WebP file should not be empty");
    }

    #[test]
    fn save_avif_writes_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let img = image::DynamicImage::ImageRgba8(
            image::ImageBuffer::from_fn(10, 10, |_, _| image::Rgba([0, 255, 0, 255]))
        );
        let path = tmp.path().join("test.avif");
        save_avif(&img, &path, 50).expect("save_avif should succeed");
        assert!(path.exists(), "AVIF file should be created");
        assert!(std::fs::metadata(&path).unwrap().len() > 0, "AVIF file should not be empty");
    }

    #[test]
    fn process_upload_rejects_invalid_mime() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = UploadedFile {
            filename: "test.txt".into(),
            content_type: "text/plain".into(),
            data: b"hello".to_vec(),
        };
        let config = CollectionUpload {
            enabled: true,
            mime_types: vec!["image/*".into()],
            ..Default::default()
        };
        let result = process_upload(&file, &config, tmp.path(), "posts", 50 * 1024 * 1024);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("text/plain"), "Error should mention the rejected MIME type");
    }

    #[test]
    fn process_upload_rejects_oversized_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = UploadedFile {
            filename: "big.bin".into(),
            content_type: "application/octet-stream".into(),
            data: vec![0u8; 1024], // 1KB
        };
        let config = CollectionUpload {
            enabled: true,
            max_file_size: Some(512), // only allow 512 bytes
            ..Default::default()
        };
        let result = process_upload(&file, &config, tmp.path(), "posts", 50 * 1024 * 1024);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("exceeds"), "Error should mention size exceeded");
    }

    #[test]
    fn process_upload_uses_global_max_when_no_per_collection_limit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = UploadedFile {
            filename: "big.bin".into(),
            content_type: "application/octet-stream".into(),
            data: vec![0u8; 1024], // 1KB
        };
        let config = CollectionUpload {
            enabled: true,
            max_file_size: None, // use global
            ..Default::default()
        };
        // Global max is 512 bytes
        let result = process_upload(&file, &config, tmp.path(), "posts", 512);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exceeds"));
    }

    #[test]
    fn process_upload_non_image_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = UploadedFile {
            filename: "document.pdf".into(),
            content_type: "application/pdf".into(),
            data: b"%PDF-1.4 test content".to_vec(),
        };
        let config = CollectionUpload {
            enabled: true,
            ..Default::default()
        };
        let result = process_upload(&file, &config, tmp.path(), "docs", 50 * 1024 * 1024)
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
        let file = UploadedFile {
            filename: "photo.png".into(),
            content_type: "image/png".into(),
            data: png_data,
        };
        let config = CollectionUpload {
            enabled: true,
            ..Default::default()
        };
        let result = process_upload(&file, &config, tmp.path(), "media", 50 * 1024 * 1024)
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
        let file = UploadedFile {
            filename: "photo.png".into(),
            content_type: "image/png".into(),
            data: png_data,
        };
        let config = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSize { name: "thumb".into(), width: 50, height: 50, fit: ImageFit::Cover },
            ],
            ..Default::default()
        };
        let result = process_upload(&file, &config, tmp.path(), "media", 50 * 1024 * 1024)
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
        let file = UploadedFile {
            filename: "photo.png".into(),
            content_type: "image/png".into(),
            data: png_data,
        };
        let config = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSize { name: "small".into(), width: 30, height: 30, fit: ImageFit::Cover },
            ],
            format_options: FormatOptions {
                webp: Some(FormatQuality { quality: 80, queue: false }),
                avif: None,
            },
            ..Default::default()
        };
        let result = process_upload(&file, &config, tmp.path(), "media", 50 * 1024 * 1024)
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
        let file = UploadedFile {
            filename: "photo.png".into(),
            content_type: "image/png".into(),
            data: png_data,
        };
        let config = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSize { name: "small".into(), width: 30, height: 30, fit: ImageFit::Cover },
            ],
            format_options: FormatOptions {
                webp: None,
                avif: Some(FormatQuality { quality: 50, queue: false }),
            },
            ..Default::default()
        };
        let result = process_upload(&file, &config, tmp.path(), "media", 50 * 1024 * 1024)
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
        let file = UploadedFile {
            filename: "photo.png".into(),
            content_type: "image/png".into(),
            data: png_data,
        };
        let config = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSize { name: "icon".into(), width: 20, height: 20, fit: ImageFit::Fill },
            ],
            format_options: FormatOptions {
                webp: Some(FormatQuality { quality: 80, queue: false }),
                avif: Some(FormatQuality { quality: 50, queue: false }),
            },
            ..Default::default()
        };
        let result = process_upload(&file, &config, tmp.path(), "media", 50 * 1024 * 1024)
            .expect("should succeed");
        let icon = &result.sizes["icon"];
        assert!(icon.formats.contains_key("webp"));
        assert!(icon.formats.contains_key("avif"));
    }

    #[test]
    fn process_upload_filename_without_extension() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Test with a non-image file that has no extension
        let file = UploadedFile {
            filename: "noext".into(),
            content_type: "application/octet-stream".into(),
            data: b"binary data".to_vec(),
        };
        let config = CollectionUpload {
            enabled: true,
            ..Default::default()
        };
        let result = process_upload(&file, &config, tmp.path(), "media", 50 * 1024 * 1024)
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
        let file = UploadedFile {
            filename: "test.png".into(),
            content_type: "image/png".into(),
            data: png_data,
        };
        let config = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSize { name: "thumb".into(), width: 30, height: 30, fit: ImageFit::Cover },
            ],
            ..Default::default()
        };
        let result = process_upload(&file, &config, tmp.path(), "media", 50 * 1024 * 1024)
            .expect("should succeed");
        let thumb = &result.sizes["thumb"];
        assert!(thumb.url.ends_with("_thumb.png"), "Size URL should have .png extension: {}", thumb.url);
    }

    #[test]
    fn inject_upload_metadata_basic() {
        let processed = ProcessedUpload {
            filename: "abc_photo.png".into(),
            mime_type: "image/png".into(),
            filesize: 12345,
            width: Some(800),
            height: Some(600),
            url: "/uploads/media/abc_photo.png".into(),
            sizes: HashMap::new(),
            queued_conversions: Vec::new(),
        };
        let mut form_data = HashMap::new();
        inject_upload_metadata(&mut form_data, &processed);

        assert_eq!(form_data.get("filename").unwrap(), "abc_photo.png");
        assert_eq!(form_data.get("mime_type").unwrap(), "image/png");
        assert_eq!(form_data.get("filesize").unwrap(), "12345");
        assert_eq!(form_data.get("width").unwrap(), "800");
        assert_eq!(form_data.get("height").unwrap(), "600");
        assert_eq!(form_data.get("url").unwrap(), "/uploads/media/abc_photo.png");
    }

    #[test]
    fn inject_upload_metadata_no_dimensions() {
        let processed = ProcessedUpload {
            filename: "doc.pdf".into(),
            mime_type: "application/pdf".into(),
            filesize: 999,
            width: None,
            height: None,
            url: "/uploads/docs/doc.pdf".into(),
            sizes: HashMap::new(),
            queued_conversions: Vec::new(),
        };
        let mut form_data = HashMap::new();
        inject_upload_metadata(&mut form_data, &processed);

        assert!(!form_data.contains_key("width"));
        assert!(!form_data.contains_key("height"));
        assert_eq!(form_data.get("filename").unwrap(), "doc.pdf");
    }

    #[test]
    fn inject_upload_metadata_with_sizes() {
        let mut sizes = HashMap::new();
        let mut formats = HashMap::new();
        formats.insert("webp".into(), FormatResult { url: "/uploads/m/t.webp".into() });
        sizes.insert("thumb".into(), SizeResult {
            url: "/uploads/m/t.png".into(),
            width: 100,
            height: 100,
            formats,
        });

        let processed = ProcessedUpload {
            filename: "img.png".into(),
            mime_type: "image/png".into(),
            filesize: 5000,
            width: Some(800),
            height: Some(600),
            url: "/uploads/m/img.png".into(),
            sizes,
            queued_conversions: Vec::new(),
        };
        let mut form_data = HashMap::new();
        inject_upload_metadata(&mut form_data, &processed);

        assert_eq!(form_data.get("thumb_url").unwrap(), "/uploads/m/t.png");
        assert_eq!(form_data.get("thumb_width").unwrap(), "100");
        assert_eq!(form_data.get("thumb_height").unwrap(), "100");
        assert_eq!(form_data.get("thumb_webp_url").unwrap(), "/uploads/m/t.webp");
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
        doc_fields.insert("url".into(), serde_json::json!("/uploads/media/nonexistent.png"));

        // Should not panic even if file doesn't exist
        delete_upload_files(tmp.path(), &doc_fields);
    }

    #[test]
    fn delete_upload_files_skips_non_upload_urls() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut doc_fields = HashMap::new();
        doc_fields.insert("url".into(), serde_json::json!("https://external.com/image.png"));
        doc_fields.insert("website_url".into(), serde_json::json!("https://example.com"));

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
        doc_fields.insert("thumb_url".into(), serde_json::json!("/uploads/media/orig_thumb.png"));
        doc_fields.insert("thumb_webp_url".into(), serde_json::json!("/uploads/media/orig_thumb.webp"));

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
        doc_fields.insert("image_url".into(), serde_json::json!("/uploads/media/keep.png"));

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

    #[test]
    fn assemble_sizes_with_avif_format() {
        use crate::core::Document;

        let upload = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSize { name: "thumb".into(), width: 100, height: 100, fit: ImageFit::Cover },
            ],
            format_options: FormatOptions {
                webp: None,
                avif: Some(FormatQuality { quality: 50, queue: false }),
            },
            ..Default::default()
        };

        let mut doc = Document::new("id1".into());
        doc.fields.insert("thumb_url".into(), serde_json::json!("/uploads/m/t.png"));
        doc.fields.insert("thumb_width".into(), serde_json::json!(100));
        doc.fields.insert("thumb_height".into(), serde_json::json!(100));
        doc.fields.insert("thumb_avif_url".into(), serde_json::json!("/uploads/m/t.avif"));

        assemble_sizes_object(&mut doc, &upload);

        let sizes = doc.fields.get("sizes").expect("sizes should exist");
        let thumb = sizes.get("thumb").expect("thumb");
        let formats = thumb.get("formats").expect("formats");
        assert!(formats.get("avif").is_some(), "AVIF format should be in assembled object");
        assert_eq!(
            formats.get("avif").unwrap().get("url").unwrap().as_str().unwrap(),
            "/uploads/m/t.avif"
        );
        // webp should not be present
        assert!(formats.get("webp").is_none());
    }

    #[test]
    fn assemble_sizes_missing_url_cleans_format_columns() {
        use crate::core::Document;

        let upload = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSize { name: "thumb".into(), width: 100, height: 100, fit: ImageFit::Cover },
            ],
            format_options: FormatOptions {
                webp: Some(FormatQuality { quality: 80, queue: false }),
                avif: Some(FormatQuality { quality: 50, queue: false }),
            },
            ..Default::default()
        };

        let mut doc = Document::new("id1".into());
        // No thumb_url, but format columns exist (edge case: orphaned format columns)
        doc.fields.insert("thumb_webp_url".into(), serde_json::json!("/uploads/m/t.webp"));
        doc.fields.insert("thumb_avif_url".into(), serde_json::json!("/uploads/m/t.avif"));

        assemble_sizes_object(&mut doc, &upload);

        // The else branch should remove format columns even without URL
        assert!(!doc.fields.contains_key("thumb_webp_url"), "Orphaned webp column should be removed");
        assert!(!doc.fields.contains_key("thumb_avif_url"), "Orphaned avif column should be removed");
        assert!(!doc.fields.contains_key("sizes"), "No sizes object since no URL");
    }

    #[test]
    fn assemble_sizes_partial_dimensions() {
        use crate::core::Document;

        let upload = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSize { name: "thumb".into(), width: 100, height: 100, fit: ImageFit::Cover },
            ],
            format_options: FormatOptions::default(),
            ..Default::default()
        };

        let mut doc = Document::new("id1".into());
        doc.fields.insert("thumb_url".into(), serde_json::json!("/uploads/m/t.png"));
        // Only width, no height
        doc.fields.insert("thumb_width".into(), serde_json::json!(100));

        assemble_sizes_object(&mut doc, &upload);

        let sizes = doc.fields.get("sizes").expect("sizes");
        let thumb = sizes.get("thumb").expect("thumb");
        assert!(thumb.get("width").is_some());
        assert!(thumb.get("height").is_none(), "Missing height should not appear");
        // No formats since format_options is default (None)
        assert!(thumb.get("formats").is_none());
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

    #[test]
    fn collection_upload_default() {
        let upload = CollectionUpload::default();
        assert!(!upload.enabled);
        assert!(upload.mime_types.is_empty());
        assert!(upload.max_file_size.is_none());
        assert!(upload.image_sizes.is_empty());
        assert!(upload.admin_thumbnail.is_none());
        assert!(upload.format_options.webp.is_none());
        assert!(upload.format_options.avif.is_none());
    }

    #[test]
    fn image_fit_default_is_cover() {
        let fit = ImageFit::default();
        assert!(matches!(fit, ImageFit::Cover));
    }

    #[test]
    fn resize_image_cover_exact_ratio() {
        // Source and target have exact same aspect ratio — should just resize, no crop
        let img = image::DynamicImage::ImageRgba8(
            image::ImageBuffer::from_fn(200, 100, |_, _| image::Rgba([0, 0, 0, 255]))
        );
        let size = ImageSize {
            name: "thumb".into(),
            width: 100,
            height: 50,
            fit: ImageFit::Cover,
        };
        let result = resize_image(&img, &size);
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 50);
    }

    #[test]
    fn process_upload_queue_mode_defers_format_conversion() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let png_data = create_test_png(80, 80);
        let file = UploadedFile {
            filename: "photo.png".into(),
            content_type: "image/png".into(),
            data: png_data,
        };
        let config = CollectionUpload {
            enabled: true,
            image_sizes: vec![
                ImageSize { name: "small".into(), width: 30, height: 30, fit: ImageFit::Cover },
            ],
            format_options: FormatOptions {
                webp: Some(FormatQuality { quality: 80, queue: true }),
                avif: Some(FormatQuality { quality: 50, queue: true }),
            },
            ..Default::default()
        };
        let result = process_upload(&file, &config, tmp.path(), "media", 50 * 1024 * 1024)
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
    fn process_image_entry_converts_webp() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let png_data = create_test_png(30, 30);
        let source = tmp.path().join("source.png");
        std::fs::write(&source, &png_data).unwrap();

        // Save the source as a proper image file first
        let img = image::load_from_memory(&png_data).unwrap();
        img.save(&source).unwrap();

        let target = tmp.path().join("output.webp");
        process_image_entry(
            source.to_str().unwrap(),
            target.to_str().unwrap(),
            "webp",
            80,
        ).expect("WebP conversion should succeed");

        assert!(target.exists(), "WebP file should be created");
        assert!(target.metadata().unwrap().len() > 0);
    }

    #[test]
    fn process_image_entry_converts_avif() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let png_data = create_test_png(30, 30);
        let source = tmp.path().join("source.png");
        let img = image::load_from_memory(&png_data).unwrap();
        img.save(&source).unwrap();

        let target = tmp.path().join("output.avif");
        process_image_entry(
            source.to_str().unwrap(),
            target.to_str().unwrap(),
            "avif",
            50,
        ).expect("AVIF conversion should succeed");

        assert!(target.exists(), "AVIF file should be created");
    }

    #[test]
    fn process_image_entry_missing_source_fails() {
        let result = process_image_entry("/nonexistent/file.png", "/tmp/out.webp", "webp", 80);
        assert!(result.is_err());
    }

    #[test]
    fn process_image_entry_unknown_format_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let png_data = create_test_png(10, 10);
        let source = tmp.path().join("source.png");
        let img = image::load_from_memory(&png_data).unwrap();
        img.save(&source).unwrap();

        let result = process_image_entry(
            source.to_str().unwrap(),
            tmp.path().join("out.xyz").to_str().unwrap(),
            "xyz",
            80,
        );
        assert!(result.is_err());
    }
}
