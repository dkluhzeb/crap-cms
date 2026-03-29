use std::{fs, io::Cursor, path::Path};

use anyhow::{Context as _, Result, bail};

use crate::core::upload::{ImageFit, ImageSize};

/// Resize an image according to the given size definition and fit mode.
///
/// Returns `None` if the source image has zero width or height (malformed image).
pub(super) fn resize_image(
    img: &image::DynamicImage,
    size: &ImageSize,
) -> Option<image::DynamicImage> {
    if img.width() == 0 || img.height() == 0 {
        return None;
    }

    let filter = image::imageops::FilterType::CatmullRom;
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

/// Save image as lossy WebP with given quality (via libwebp).
pub(super) fn save_webp(img: &image::DynamicImage, path: &Path, quality: u8) -> Result<()> {
    let rgba = img.to_rgba8();
    let encoder = webp::Encoder::from_rgba(&rgba, img.width(), img.height());
    let mem = encoder.encode(quality as f32);
    fs::write(path, &*mem).with_context(|| format!("Failed to write WebP: {}", path.display()))?;
    Ok(())
}

/// Save image as AVIF with given quality.
pub(super) fn save_avif(img: &image::DynamicImage, path: &Path, quality: u8) -> Result<()> {
    use image::ImageEncoder;
    let rgba = img.to_rgba8();
    let mut buf = Cursor::new(Vec::new());
    let encoder = image::codecs::avif::AvifEncoder::new_with_speed_quality(&mut buf, 8, quality);
    encoder
        .write_image(
            rgba.as_raw(),
            img.width(),
            img.height(),
            image::ExtendedColorType::Rgba8,
        )
        .with_context(|| "Failed to encode AVIF")?;
    fs::write(path, buf.into_inner())
        .with_context(|| format!("Failed to write AVIF: {}", path.display()))?;
    Ok(())
}

/// Process a single image queue entry: read source, convert to target format, save to disk.
/// Returns Ok(()) on success, Err on failure.
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
        // We use moderate dimensions to avoid huge memory allocations during
        // the actual resize, while still exercising the ratio math.
        let img = image::DynamicImage::ImageRgba8(image::ImageBuffer::from_fn(1000, 1, |_, _| {
            image::Rgba([0, 0, 0, 255])
        }));
        let size = ImageSizeBuilder::new("extreme")
            .width(1)
            .height(1000)
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
