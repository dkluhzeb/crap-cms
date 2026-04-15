use std::{collections::HashMap, io::Cursor};
#[cfg(test)]
use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};
use image::{
    DynamicImage, ExtendedColorType, ImageEncoder, ImageFormat, codecs::avif::AvifEncoder, imageops,
};
use tracing::warn;

use crate::core::upload::{
    CollectionUpload, FormatQuality, FormatResult, ImageFit, ImageSize, QueuedConversion,
    QueuedConversionBuilder, SharedStorage, SizeResult, SizeResultBuilder,
};

use super::{StorageBackend, process::CleanupGuard};

/// Resize an image according to the given size definition and fit mode.
///
/// Returns `None` if the source image has zero width or height (malformed image).
pub(super) fn resize_image(img: &DynamicImage, size: &ImageSize) -> Option<DynamicImage> {
    if img.width() == 0 || img.height() == 0 {
        return None;
    }

    let filter = imageops::FilterType::CatmullRom;
    Some(match size.fit {
        ImageFit::Cover => {
            // Resize to fill, then center crop
            let src_ratio = img.width() as f64 / img.height() as f64;
            let dst_ratio = size.width as f64 / size.height as f64;

            let (resize_w, resize_h) = if src_ratio > dst_ratio {
                // Source is wider — fit height, crop width
                let h = size.height;
                let w = (img.width() as f64 * (size.height as f64 / img.height() as f64))
                    .min(u32::MAX as f64) as u32;
                (w.max(1), h)
            } else {
                // Source is taller — fit width, crop height
                let w = size.width;
                let h = (img.height() as f64 * (size.width as f64 / img.width() as f64))
                    .min(u32::MAX as f64) as u32;
                (w, h.max(1))
            };

            let resized = img.resize_exact(resize_w, resize_h, filter);
            let x = (resized.width().saturating_sub(size.width)) / 2;
            let y = (resized.height().saturating_sub(size.height)) / 2;

            resized.crop_imm(
                x,
                y,
                size.width.min(resized.width()),
                size.height.min(resized.height()),
            )
        }
        ImageFit::Contain | ImageFit::Inside => {
            // Resize to fit within bounds, preserving aspect ratio
            img.resize(size.width, size.height, filter)
        }
        ImageFit::Fill => {
            // Stretch to exact dimensions
            img.resize_exact(size.width, size.height, filter)
        }
    })
}

/// Encode image as lossy WebP with given quality (via libwebp), returning raw bytes.
pub(super) fn webp_to_bytes(img: &DynamicImage, quality: u8) -> Vec<u8> {
    let rgba = img.to_rgba8();
    let encoder = webp::Encoder::from_rgba(&rgba, img.width(), img.height());
    let mem = encoder.encode(quality as f32);

    mem.to_vec()
}

/// Save image as lossy WebP with given quality (via libwebp).
#[cfg(test)]
pub(super) fn save_webp(img: &DynamicImage, path: &Path, quality: u8) -> Result<()> {
    let data = webp_to_bytes(img, quality);

    fs::write(path, &data).with_context(|| format!("Failed to write WebP: {}", path.display()))?;

    Ok(())
}

/// Encode image as AVIF with given quality, returning raw bytes.
pub(super) fn avif_to_bytes(img: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let rgba = img.to_rgba8();
    let mut buf = Cursor::new(Vec::new());
    let encoder = AvifEncoder::new_with_speed_quality(&mut buf, 8, quality);

    encoder
        .write_image(
            rgba.as_raw(),
            img.width(),
            img.height(),
            ExtendedColorType::Rgba8,
        )
        .with_context(|| "Failed to encode AVIF")?;

    Ok(buf.into_inner())
}

/// Save image as AVIF with given quality.
#[cfg(test)]
pub(super) fn save_avif(img: &DynamicImage, path: &Path, quality: u8) -> Result<()> {
    let data = avif_to_bytes(img, quality)?;

    fs::write(path, &data).with_context(|| format!("Failed to write AVIF: {}", path.display()))?;

    Ok(())
}

/// Process a single image queue entry: read source, convert to target format, save to disk.
/// Returns Ok(()) on success, Err on failure.
/// Process a queued image conversion using storage backend.
/// `source_key` and `target_key` are storage keys (or filesystem paths for local).
pub fn process_image_entry_with_storage(
    source_key: &str,
    target_key: &str,
    format: &str,
    quality: u8,
    storage: &dyn StorageBackend,
) -> Result<()> {
    let source_data = storage
        .get(source_key)
        .with_context(|| format!("Source image not found: {}", source_key))?;

    let img = image::load_from_memory(&source_data)
        .with_context(|| format!("Failed to decode image: {}", source_key))?;

    let target_data = match format {
        "webp" => webp_to_bytes(&img, quality),
        "avif" => avif_to_bytes(&img, quality)?,
        _ => bail!("Unsupported format: {}", format),
    };

    let content_type = match format {
        "webp" => "image/webp",
        "avif" => "image/avif",
        _ => "application/octet-stream",
    };

    storage
        .put(target_key, &target_data, content_type)
        .with_context(|| format!("Failed to save converted image: {}", target_key))?;

    Ok(())
}

/// Save a resized image to storage and return `(size_key, size_url)`.
pub(super) fn save_resized_image(
    resized: &DynamicImage,
    stem: &str,
    ext: &str,
    size_name: &str,
    collection_slug: &str,
    storage: &SharedStorage,
    guard: &mut CleanupGuard,
) -> Result<(String, String)> {
    let size_filename = format!("{}_{}.{}", stem, size_name, ext);
    let size_key = format!("{}/{}", collection_slug, size_filename);

    let mut buf = Cursor::new(Vec::new());

    resized
        .write_to(
            &mut buf,
            ImageFormat::from_extension(ext).unwrap_or(ImageFormat::Png),
        )
        .with_context(|| format!("Failed to encode resized image: {}", size_key))?;

    let size_mime = mime_guess::from_path(&size_filename)
        .first_or_octet_stream()
        .to_string();

    storage
        .put(&size_key, &buf.into_inner(), &size_mime)
        .with_context(|| format!("Failed to save resized image: {}", size_key))?;

    guard.push(size_key.clone());

    let size_url = format!("/uploads/{}", size_key);

    Ok((size_key, size_url))
}

/// Context for processing a format variant of a resized image.
pub(super) struct FormatVariantCtx<'a> {
    pub resized: &'a DynamicImage,
    pub format_name: &'a str,
    pub opts: &'a FormatQuality,
    pub stem: &'a str,
    pub size_name: &'a str,
    pub size_key: &'a str,
    pub collection_slug: &'a str,
    pub storage: &'a SharedStorage,
}

/// Process a format variant (WebP or AVIF) for a resized image.
/// Either saves immediately or queues for async conversion.
pub(super) fn process_format_variant(
    ctx: &FormatVariantCtx<'_>,
    guard: &mut CleanupGuard,
    formats: &mut HashMap<String, FormatResult>,
    queued: &mut Vec<QueuedConversion>,
) -> Result<()> {
    let variant_filename = format!("{}_{}.{}", ctx.stem, ctx.size_name, ctx.format_name);
    let variant_key = format!("{}/{}", ctx.collection_slug, variant_filename);
    let variant_url = format!("/uploads/{}", variant_key);

    if ctx.opts.queue {
        let source_path = ctx
            .storage
            .local_path(ctx.size_key)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| ctx.size_key.to_string());

        let target_path = ctx
            .storage
            .local_path(&variant_key)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| variant_key.clone());

        queued.push(
            QueuedConversionBuilder::new(source_path, target_path)
                .format(ctx.format_name)
                .quality(ctx.opts.quality)
                .url_column(format!("{}_{}_url", ctx.size_name, ctx.format_name))
                .url_value(variant_url)
                .build(),
        );
    } else {
        let data = match ctx.format_name {
            "webp" => webp_to_bytes(ctx.resized, ctx.opts.quality),
            "avif" => avif_to_bytes(ctx.resized, ctx.opts.quality)?,
            _ => bail!("Unknown format: {}", ctx.format_name),
        };

        let mime = format!("image/{}", ctx.format_name);

        ctx.storage
            .put(&variant_key, &data, &mime)
            .with_context(|| format!("Failed to save {}: {}", ctx.format_name, variant_key))?;

        guard.push(variant_key);

        formats.insert(ctx.format_name.to_string(), FormatResult::new(variant_url));
    }

    Ok(())
}

/// Process all image sizes and their format variants.
pub(super) fn process_image_sizes(
    img: &DynamicImage,
    unique_filename: &str,
    collection_slug: &str,
    upload_config: &CollectionUpload,
    storage: &SharedStorage,
    guard: &mut CleanupGuard,
) -> Result<(HashMap<String, SizeResult>, Vec<QueuedConversion>)> {
    let mut sizes = HashMap::new();
    let mut queued_conversions = Vec::new();

    let (stem, ext) = unique_filename
        .rsplit_once('.')
        .unwrap_or((unique_filename, "bin"));

    for size_def in &upload_config.image_sizes {
        let resized = match resize_image(img, size_def) {
            Some(r) => r,
            None => {
                warn!(
                    "Skipping size '{}' — source image has zero dimensions",
                    size_def.name
                );

                continue;
            }
        };

        let (size_key, size_url) = save_resized_image(
            &resized,
            stem,
            ext,
            &size_def.name,
            collection_slug,
            storage,
            guard,
        )?;

        let mut formats = HashMap::new();

        if let Some(ref webp_opts) = upload_config.format_options.webp {
            let ctx = FormatVariantCtx {
                resized: &resized,
                format_name: "webp",
                opts: webp_opts,
                stem,
                size_name: &size_def.name,
                size_key: &size_key,
                collection_slug,
                storage,
            };

            process_format_variant(&ctx, guard, &mut formats, &mut queued_conversions)?;
        }

        if let Some(ref avif_opts) = upload_config.format_options.avif {
            let ctx = FormatVariantCtx {
                resized: &resized,
                format_name: "avif",
                opts: avif_opts,
                stem,
                size_name: &size_def.name,
                size_key: &size_key,
                collection_slug,
                storage,
            };

            process_format_variant(&ctx, guard, &mut formats, &mut queued_conversions)?;
        }

        sizes.insert(
            size_def.name.clone(),
            SizeResultBuilder::new(size_url)
                .width(resized.width())
                .height(resized.height())
                .formats(formats)
                .build(),
        );
    }

    Ok((sizes, queued_conversions))
}

/// Process a queued image conversion using local filesystem paths.
/// Only used in tests — production code uses `process_image_entry_with_storage`.
#[cfg(test)]
pub fn process_image_entry(
    source_path: &str,
    target_path: &str,
    format: &str,
    quality: u8,
) -> Result<()> {
    let source = Path::new(source_path);

    if !source.exists() {
        bail!("Source image not found: {}", source_path);
    }

    let img =
        image::open(source).with_context(|| format!("Failed to decode image: {}", source_path))?;

    let target = Path::new(target_path);

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }

    match format {
        "webp" => save_webp(&img, target, quality)?,
        "avif" => save_avif(&img, target, quality)?,
        _ => bail!("Unsupported format: {}", format),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::core::upload::ImageSizeBuilder;

    /// Create a small test PNG image in memory.
    fn create_test_png(width: u32, height: u32) -> Vec<u8> {
        use image::{ImageBuffer, ImageEncoder, Rgba};
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

    #[test]
    fn resize_image_cover_wider_source() {
        // Source is wider than target aspect ratio (landscape → square crop)
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(400, 200, |_, _| {
            image::Rgba([0, 0, 0, 255])
        }));
        let size = ImageSizeBuilder::new("thumb")
            .width(100)
            .height(100)
            .fit(ImageFit::Cover)
            .build();
        let result = resize_image(&img, &size).unwrap();
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 100);
    }

    #[test]
    fn resize_image_cover_taller_source() {
        // Source is taller than target aspect ratio (portrait → square crop)
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(200, 400, |_, _| {
            image::Rgba([0, 0, 0, 255])
        }));
        let size = ImageSizeBuilder::new("thumb")
            .width(100)
            .height(100)
            .fit(ImageFit::Cover)
            .build();
        let result = resize_image(&img, &size).unwrap();
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 100);
    }

    #[test]
    fn resize_image_contain() {
        // Contain: fits within bounds, preserving aspect ratio
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(400, 200, |_, _| {
            image::Rgba([0, 0, 0, 255])
        }));
        let size = ImageSizeBuilder::new("card")
            .width(100)
            .height(100)
            .fit(ImageFit::Contain)
            .build();
        let result = resize_image(&img, &size).unwrap();
        // Should fit within 100x100 preserving 2:1 aspect → 100x50
        assert!(result.width() <= 100);
        assert!(result.height() <= 100);
        // The wider dimension should hit the limit
        assert_eq!(result.width(), 100);
    }

    #[test]
    fn resize_image_inside() {
        // Inside: same as contain (fits within bounds)
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(200, 400, |_, _| {
            image::Rgba([0, 0, 0, 255])
        }));
        let size = ImageSizeBuilder::new("card")
            .width(100)
            .height(100)
            .fit(ImageFit::Inside)
            .build();
        let result = resize_image(&img, &size).unwrap();
        assert!(result.width() <= 100);
        assert!(result.height() <= 100);
    }

    #[test]
    fn resize_image_fill() {
        // Fill: stretch to exact dimensions, ignoring aspect ratio
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(400, 200, |_, _| {
            image::Rgba([0, 0, 0, 255])
        }));
        let size = ImageSizeBuilder::new("banner")
            .width(150)
            .height(75)
            .fit(ImageFit::Fill)
            .build();
        let result = resize_image(&img, &size).unwrap();
        assert_eq!(result.width(), 150);
        assert_eq!(result.height(), 75);
    }

    #[test]
    fn resize_image_cover_exact_ratio() {
        // Source and target have exact same aspect ratio — should just resize, no crop
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(200, 100, |_, _| {
            image::Rgba([0, 0, 0, 255])
        }));
        let size = ImageSizeBuilder::new("thumb")
            .width(100)
            .height(50)
            .fit(ImageFit::Cover)
            .build();
        let result = resize_image(&img, &size).unwrap();
        assert_eq!(result.width(), 100);
        assert_eq!(result.height(), 50);
    }

    #[test]
    fn resize_image_cover_extreme_aspect_ratio_no_overflow() {
        // Wide source with tall target — the intermediate width calculation
        // could overflow u32 without the .min(u32::MAX) guard.
        // Use small dimensions (10x1 → 1x10) to exercise the ratio math
        // without allocating a huge intermediate image.
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(10, 1, |_, _| {
            image::Rgba([0, 0, 0, 255])
        }));
        let size = ImageSizeBuilder::new("extreme")
            .width(1)
            .height(10)
            .fit(ImageFit::Cover)
            .build();

        // Should not panic from overflow
        let result = resize_image(&img, &size).unwrap();
        assert!(result.width() >= 1);
        assert!(result.height() >= 1);
    }

    #[test]
    fn resize_image_returns_none_for_zero_dimensions() {
        let img_zero_height =
            image::DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(100, 0, |_, _| {
                image::Rgba([0, 0, 0, 255])
            }));
        let img_zero_width =
            image::DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(0, 100, |_, _| {
                image::Rgba([0, 0, 0, 255])
            }));
        let size = ImageSizeBuilder::new("thumb")
            .width(50)
            .height(50)
            .fit(ImageFit::Cover)
            .build();

        assert!(
            resize_image(&img_zero_height, &size).is_none(),
            "Zero-height image should return None"
        );
        assert!(
            resize_image(&img_zero_width, &size).is_none(),
            "Zero-width image should return None"
        );
    }

    #[test]
    fn save_webp_writes_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(10, 10, |_, _| {
            image::Rgba([255, 0, 0, 255])
        }));
        let path = tmp.path().join("test.webp");
        save_webp(&img, &path, 80).expect("save_webp should succeed");
        assert!(path.exists(), "WebP file should be created");
        assert!(
            fs::metadata(&path).unwrap().len() > 0,
            "WebP file should not be empty"
        );
    }

    #[test]
    fn save_avif_writes_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(10, 10, |_, _| {
            image::Rgba([0, 255, 0, 255])
        }));
        let path = tmp.path().join("test.avif");
        save_avif(&img, &path, 50).expect("save_avif should succeed");
        assert!(path.exists(), "AVIF file should be created");
        assert!(
            fs::metadata(&path).unwrap().len() > 0,
            "AVIF file should not be empty"
        );
    }

    #[test]
    fn process_image_entry_converts_webp() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let png_data = create_test_png(30, 30);
        let source = tmp.path().join("source.png");
        fs::write(&source, &png_data).unwrap();

        // Save the source as a proper image file first
        let img = image::load_from_memory(&png_data).unwrap();
        img.save(&source).unwrap();

        let target = tmp.path().join("output.webp");
        process_image_entry(
            source.to_str().unwrap(),
            target.to_str().unwrap(),
            "webp",
            80,
        )
        .expect("WebP conversion should succeed");

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
        )
        .expect("AVIF conversion should succeed");

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
