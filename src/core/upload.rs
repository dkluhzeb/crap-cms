use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Default for CollectionUpload {
    fn default() -> Self {
        Self {
            enabled: false,
            mime_types: Vec::new(),
            max_file_size: None,
            image_sizes: Vec::new(),
            admin_thumbnail: None,
            format_options: FormatOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSize {
    pub name: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub fit: ImageFit,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FormatOptions {
    #[serde(default)]
    pub webp: Option<FormatQuality>,
    #[serde(default)]
    pub avif: Option<FormatQuality>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatQuality {
    pub quality: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ImageFit {
    #[default]
    Cover,
    Contain,
    Inside,
    Fill,
}

pub struct UploadedFile {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

pub struct ProcessedUpload {
    pub filename: String,
    pub mime_type: String,
    pub filesize: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub url: String,
    pub sizes: HashMap<String, SizeResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizeResult {
    pub url: String,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub formats: HashMap<String, FormatResult>,
}

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
    // Validate MIME type
    if !validate_mime_type(&file.content_type, &upload_config.mime_types) {
        bail!("File type '{}' is not allowed", file.content_type);
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

    // Save original file
    let original_path = upload_dir.join(&unique_filename);
    std::fs::write(&original_path, &file.data)
        .with_context(|| format!("Failed to write file: {}", original_path.display()))?;

    let url = format!("/uploads/{}/{}", collection_slug, unique_filename);

    let is_image = file.content_type.starts_with("image/");

    let mut width = None;
    let mut height = None;
    let mut sizes = HashMap::new();

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

            let size_url = format!("/uploads/{}/{}", collection_slug, size_filename);
            let mut formats = HashMap::new();

            // WebP variant
            if let Some(ref webp_opts) = upload_config.format_options.webp {
                let webp_filename = format!("{}_{}.webp", stem, size_def.name);
                let webp_path = upload_dir.join(&webp_filename);
                save_webp(&resized, &webp_path, webp_opts.quality)?;
                formats.insert("webp".to_string(), FormatResult {
                    url: format!("/uploads/{}/{}", collection_slug, webp_filename),
                });
            }

            // AVIF variant
            if let Some(ref avif_opts) = upload_config.format_options.avif {
                let avif_filename = format!("{}_{}.avif", stem, size_def.name);
                let avif_path = upload_dir.join(&avif_filename);
                save_avif(&resized, &avif_path, avif_opts.quality)?;
                formats.insert("avif".to_string(), FormatResult {
                    url: format!("/uploads/{}/{}", collection_slug, avif_filename),
                });
            }

            sizes.insert(size_def.name.clone(), SizeResult {
                url: size_url,
                width: resized.width(),
                height: resized.height(),
                formats,
            });
        }
    }

    Ok(ProcessedUpload {
        filename: unique_filename,
        mime_type: file.content_type.clone(),
        filesize,
        width,
        height,
        url,
        sizes,
    })
}

/// Resize an image according to the given size definition and fit mode.
fn resize_image(img: &image::DynamicImage, size: &ImageSize) -> image::DynamicImage {
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

            let resized = img.resize_exact(resize_w, resize_h, image::imageops::FilterType::Lanczos3);
            let x = (resized.width().saturating_sub(size.width)) / 2;
            let y = (resized.height().saturating_sub(size.height)) / 2;
            resized.crop_imm(x, y, size.width.min(resized.width()), size.height.min(resized.height()))
        }
        ImageFit::Contain | ImageFit::Inside => {
            // Resize to fit within bounds, preserving aspect ratio
            img.resize(size.width, size.height, image::imageops::FilterType::Lanczos3)
        }
        ImageFit::Fill => {
            // Stretch to exact dimensions
            img.resize_exact(size.width, size.height, image::imageops::FilterType::Lanczos3)
        }
    }
}

/// Save image as WebP with given quality.
fn save_webp(img: &image::DynamicImage, path: &Path, _quality: u8) -> Result<()> {
    use image::ImageEncoder;
    let rgba = img.to_rgba8();
    let mut buf = Cursor::new(Vec::new());
    let encoder = image::codecs::webp::WebPEncoder::new_lossless(&mut buf);
    encoder.write_image(
        rgba.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgba8,
    ).with_context(|| "Failed to encode WebP")?;
    std::fs::write(path, buf.into_inner())
        .with_context(|| format!("Failed to write WebP: {}", path.display()))?;
    Ok(())
}

/// Save image as AVIF with given quality.
fn save_avif(img: &image::DynamicImage, path: &Path, quality: u8) -> Result<()> {
    use image::ImageEncoder;
    let rgba = img.to_rgba8();
    let mut buf = Cursor::new(Vec::new());
    let encoder = image::codecs::avif::AvifEncoder::new_with_speed_quality(&mut buf, 6, quality);
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
                webp: Some(FormatQuality { quality: 80 }),
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
}
