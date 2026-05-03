//! EXIF orientation handling.
//!
//! Cameras and phones often record an image with raw pixel data that is
//! sideways and an EXIF `Orientation` tag (1–8) telling the renderer how
//! to display it upright. The `image` crate's decoders only return the
//! pixel data — they ignore the tag — so without explicit handling we
//! ship those photos sideways through the resize / format pipeline and
//! into the served output.
//!
//! `apply_exif_orientation` reads the orientation from the original file
//! bytes and applies the corresponding rotation / flip to the decoded
//! `DynamicImage`. The output `image` crate encoders we use (PNG, JPEG,
//! WebP, AVIF) do not write EXIF, so re-encoding strips the original
//! camera metadata (incl. GPS) as a side effect — desirable for a CMS
//! that serves uploaded photos publicly.
//!
//! Orientation values per the EXIF spec:
//! - 1: Normal (top-left)
//! - 2: Mirrored horizontally
//! - 3: Rotated 180°
//! - 4: Mirrored vertically
//! - 5: Mirrored horizontally + rotated 270° clockwise
//! - 6: Rotated 90° clockwise
//! - 7: Mirrored horizontally + rotated 90° clockwise
//! - 8: Rotated 270° clockwise

use std::io::Cursor;

use exif::{In, Reader, Tag, Value};
use image::{DynamicImage, imageops};

/// Read the EXIF `Orientation` tag from the original file bytes.
/// Returns `None` if the file has no EXIF block, no orientation tag, or
/// an unrecognized value.
pub(crate) fn read_orientation(bytes: &[u8]) -> Option<u8> {
    let exif = Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok()?;
    let field = exif.get_field(Tag::Orientation, In::PRIMARY)?;
    match field.value {
        Value::Short(ref v) => v.first().copied().map(|v| v as u8),
        _ => None,
    }
}

/// Apply an EXIF orientation value (1–8) to a decoded image, returning the
/// upright form. Unknown values pass through unchanged.
pub(crate) fn apply_orientation(img: DynamicImage, orientation: u8) -> DynamicImage {
    match orientation {
        1 => img,
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => DynamicImage::ImageRgba8(imageops::flip_horizontal(&img.rotate270())),
        6 => img.rotate90(),
        7 => DynamicImage::ImageRgba8(imageops::flip_horizontal(&img.rotate90())),
        8 => img.rotate270(),
        _ => img,
    }
}

/// Read the EXIF orientation from the original bytes (if any) and apply
/// it to the decoded image. No-op when the file has no orientation tag
/// or the tag is `1` (already upright).
pub(crate) fn apply_exif_orientation(bytes: &[u8], img: DynamicImage) -> DynamicImage {
    match read_orientation(bytes) {
        Some(o) if o > 1 && o <= 8 => apply_orientation(img, o),
        _ => img,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    /// Build a 2x4 RGBA image where the only red pixel is at (0, 0).
    /// Other pixels are transparent black. Used to verify rotation by
    /// checking where the marker ends up after applying each orientation.
    fn marker_image() -> DynamicImage {
        let buf: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_fn(2, 4, |x, y| {
            if x == 0 && y == 0 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 0, 0])
            }
        });
        DynamicImage::ImageRgba8(buf)
    }

    fn is_red(img: &DynamicImage, x: u32, y: u32) -> bool {
        let pixel = img.to_rgba8().get_pixel(x, y).0;
        pixel == [255, 0, 0, 255]
    }

    #[test]
    fn orientation_1_passthrough() {
        let img = marker_image();
        let out = apply_orientation(img, 1);
        assert_eq!(out.width(), 2);
        assert_eq!(out.height(), 4);
        assert!(is_red(&out, 0, 0));
    }

    #[test]
    fn orientation_3_rotates_180() {
        // 2x4 with marker at (0,0) → 2x4 with marker at (1,3).
        let img = marker_image();
        let out = apply_orientation(img, 3);
        assert_eq!(out.width(), 2);
        assert_eq!(out.height(), 4);
        assert!(is_red(&out, 1, 3));
    }

    #[test]
    fn orientation_6_rotates_90_cw() {
        // 2x4 with marker at (0,0) → 4x2 with marker at (3,0)
        // (top-left becomes top-right after 90° CW).
        let img = marker_image();
        let out = apply_orientation(img, 6);
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 2);
        assert!(is_red(&out, 3, 0));
    }

    #[test]
    fn orientation_8_rotates_270_cw() {
        // 2x4 with marker at (0,0) → 4x2 with marker at (0,1)
        // (top-left becomes bottom-left after 270° CW).
        let img = marker_image();
        let out = apply_orientation(img, 8);
        assert_eq!(out.width(), 4);
        assert_eq!(out.height(), 2);
        assert!(is_red(&out, 0, 1));
    }

    #[test]
    fn orientation_2_mirrors_horizontal() {
        // 2x4 with marker at (0,0) → marker at (1,0).
        let img = marker_image();
        let out = apply_orientation(img, 2);
        assert_eq!(out.width(), 2);
        assert_eq!(out.height(), 4);
        assert!(is_red(&out, 1, 0));
    }

    #[test]
    fn orientation_unknown_value_passthrough() {
        let img = marker_image();
        let out = apply_orientation(img, 42);
        assert_eq!(out.width(), 2);
        assert_eq!(out.height(), 4);
        assert!(is_red(&out, 0, 0));
    }

    /// Hand-built minimal JPEG (8x4 yellow rectangle) with an EXIF APP1
    /// marker setting `Orientation = 6` (rotate 90° CW). Verifies that the
    /// EXIF reader picks up the tag and the helper rotates the decoded
    /// image accordingly. Generated by encoding a fresh JPEG via the
    /// `image` crate then injecting an APP1/EXIF block by hand.
    #[test]
    fn end_to_end_jpeg_with_orientation_6_is_rotated() {
        // Build a tiny JPEG (8x4 — wider than tall) and encode.
        let buf: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_fn(8, 4, |x, y| {
            if x == 0 && y == 0 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([200, 200, 0, 255])
            }
        });
        let img = DynamicImage::ImageRgba8(buf);

        let mut jpeg_bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut jpeg_bytes), image::ImageFormat::Jpeg)
            .expect("encode jpeg");

        // Inject an APP1/EXIF segment with Orientation=6 between SOI and
        // the next marker. APP1 segment layout (big-endian):
        //   FF E1                      — APP1 marker
        //   <2-byte length>            — covers from length to last EXIF byte
        //   "Exif\0\0"                 — EXIF identifier (6 bytes)
        //   "MM\0*\0\0\0\x08"          — TIFF header (big-endian, IFD0 at 8)
        //   <2-byte IFD entry count>   — 1 entry
        //   <12-byte IFD entry>        — Orientation tag (0x0112), SHORT, count 1, value 6
        //   <4-byte next-IFD offset>   — 0 (no IFD1)
        let exif_payload: Vec<u8> = [
            // EXIF identifier
            b"Exif\0\0".as_ref(),
            // TIFF header — big-endian, magic 42, IFD0 offset 8
            &[0x4D, 0x4D, 0x00, 0x2A, 0x00, 0x00, 0x00, 0x08],
            // IFD0: 1 entry
            &[0x00, 0x01],
            // Tag 0x0112 (Orientation), type 3 (SHORT), count 1, value 6
            // SHORT values <= 4 bytes are inlined in the value field, big-endian.
            &[
                0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x06, 0x00, 0x00,
            ],
            // Next IFD offset: 0
            &[0x00, 0x00, 0x00, 0x00],
        ]
        .concat();

        let segment_len = (exif_payload.len() + 2) as u16; // +2 for the length field itself
        let mut app1: Vec<u8> = vec![0xFF, 0xE1, (segment_len >> 8) as u8, segment_len as u8];
        app1.extend(&exif_payload);

        // SOI is the first 2 bytes; insert APP1 right after.
        let mut with_exif = Vec::with_capacity(jpeg_bytes.len() + app1.len());
        with_exif.extend(&jpeg_bytes[..2]);
        with_exif.extend(&app1);
        with_exif.extend(&jpeg_bytes[2..]);

        // Round-trip via the EXIF reader: should pick up orientation 6.
        assert_eq!(read_orientation(&with_exif), Some(6));

        let decoded = image::load_from_memory(&with_exif).expect("decode jpeg");
        assert_eq!(decoded.width(), 8);
        assert_eq!(decoded.height(), 4);

        let upright = apply_exif_orientation(&with_exif, decoded);
        // 8x4 rotated 90° CW → 4x8.
        assert_eq!(upright.width(), 4);
        assert_eq!(upright.height(), 8);
    }

    #[test]
    fn no_exif_passes_through() {
        // PNG carries no EXIF block; apply_exif_orientation must no-op.
        let buf: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_fn(3, 5, |_, _| Rgba([0, 0, 0, 255]));
        let img = DynamicImage::ImageRgba8(buf);
        let mut png_bytes = Vec::new();
        img.write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
            .unwrap();

        assert_eq!(read_orientation(&png_bytes), None);

        let decoded = image::load_from_memory(&png_bytes).unwrap();
        let out = apply_exif_orientation(&png_bytes, decoded);
        assert_eq!(out.width(), 3);
        assert_eq!(out.height(), 5);
    }
}
