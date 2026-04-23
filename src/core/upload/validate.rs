use std::io::Cursor;

use anyhow::{Context as _, Result, bail};
use image::ImageReader;

use crate::core::upload::{CollectionUpload, UploadedFile};

/// Validate MIME type, magic bytes, and file size of an uploaded file.
pub(super) fn validate_upload(
    file: &UploadedFile,
    upload_config: &CollectionUpload,
    global_max_file_size: u64,
) -> Result<()> {
    if !validate_mime_type(&file.content_type, &upload_config.mime_types) {
        bail!("File type '{}' is not allowed", file.content_type);
    }

    // Magic-byte verification: detected type must match claimed type. When
    // `infer` recognises the bytes, the detected MIME is authoritative for
    // subsequent checks; otherwise fall back to the client-claimed type.
    let effective_mime = if let Some(detected) = infer::get(&file.data) {
        let detected_mime = detected.mime_type();

        if !mime_matches(detected_mime, &file.content_type) {
            bail!(
                "File content does not match claimed type '{}' (detected '{}')",
                file.content_type,
                detected_mime,
            );
        }

        detected_mime.to_string()
    } else {
        file.content_type.clone()
    };

    // Extension ↔ content cross-check: files are served with Content-Type
    // derived from the stored filename's extension (via `mime_guess`), so a
    // mismatch between the extension and the real content lets an attacker
    // smuggle `text/html` past an `image/*` allowlist. Reject when the
    // extension's MIME disagrees with what the bytes actually are.
    validate_filename_extension_matches(&file.filename, &effective_mime)?;

    let max_size = upload_config.max_file_size.unwrap_or(global_max_file_size);

    if file.data.len() as u64 > max_size {
        bail!(
            "File size {} exceeds maximum allowed size {}",
            format_filesize(file.data.len() as u64),
            format_filesize(max_size),
        );
    }

    Ok(())
}

/// MIME types that browsers render as executable/interpretable content —
/// the XSS surface for the H-4 attack. When the stored filename's extension
/// resolves to one of these, the actual content MUST match exactly, because
/// anything else would let an attacker smuggle active markup past an
/// `image/*` (or other innocent-looking) allowlist.
const RENDERABLE_AS_CODE_MIMES: &[&str] = &[
    "text/html",
    "application/xhtml+xml",
    "image/svg+xml",
    "text/xml",
    "application/xml",
    "application/javascript",
    "text/javascript",
];

/// Verify that the filename's extension is safe for serving given the
/// effective content type. Only "renderable" extensions (HTML, SVG, XML,
/// JS, XHTML) are strictly checked, because those are what the browser
/// would interpret as code on serve. Other extensions (txt, pdf, zip, …)
/// are served with non-executing Content-Types regardless of the actual
/// bytes, so a cosmetic mismatch there is not a security issue.
fn validate_filename_extension_matches(filename: &str, effective_mime: &str) -> Result<()> {
    let Some(dot_pos) = filename.rfind('.') else {
        return Ok(());
    };

    if dot_pos == 0 || dot_pos == filename.len() - 1 {
        // ".gitignore" (leading dot) or "foo." (trailing dot) — treat as
        // having no usable extension rather than guessing.
        return Ok(());
    }

    let ext_mime = mime_guess::from_path(filename).first_or_octet_stream();
    let ext_mime_str = ext_mime.essence_str();

    if !RENDERABLE_AS_CODE_MIMES.contains(&ext_mime_str) {
        return Ok(());
    }

    if mime_matches(ext_mime_str, effective_mime) {
        return Ok(());
    }

    bail!(
        "Filename extension implies renderable type '{}' but content is '{}' — \
         rename the file with an extension that matches its actual type",
        ext_mime_str,
        effective_mime,
    );
}

/// Check image dimensions against the decompression bomb limit (100 megapixels).
pub(super) fn check_image_dimensions(data: &[u8]) -> Result<()> {
    let reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .context("Failed to detect image format")?;

    if let Ok((w, h)) = reader.into_dimensions() {
        const MAX_PIXELS: u64 = 100_000_000;

        if (w as u64) * (h as u64) > MAX_PIXELS {
            bail!("Image too large: {}x{} exceeds pixel limit", w, h);
        }
    }

    Ok(())
}

/// Check if a content type matches a MIME glob pattern.
/// Supports patterns like "image/*", "application/pdf", etc.
pub(super) fn mime_matches(content_type: &str, pattern: &str) -> bool {
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
pub(super) fn validate_mime_type(content_type: &str, allowed: &[String]) -> bool {
    if allowed.is_empty() {
        return true;
    }
    allowed
        .iter()
        .any(|pattern| mime_matches(content_type, pattern))
}

/// Sanitize a filename: lowercase, replace non-alphanumeric with hyphens, collapse.
pub(super) fn sanitize_filename(name: &str) -> String {
    let name = name.to_lowercase();
    // Split extension from stem
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s, Some(e)),
        None => (name.as_str(), None),
    };
    let clean_stem: String = stem
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let clean_stem: String = clean_stem
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    match ext {
        Some(e) => format!("{}.{}", clean_stem, e),
        None => clean_stem,
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
    fn mime_matches_partial_type_no_slash() {
        // "image" without "/*" should not match "image/png" (exact match only)
        assert!(!mime_matches("image/png", "image"));
    }

    #[test]
    fn mime_matches_wildcard_does_not_match_without_slash() {
        // "image/*" should not match "imageextra/png" — must have "/" after prefix
        assert!(!mime_matches("imageextra/png", "image/*"));
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
    fn format_filesize_units() {
        assert_eq!(format_filesize(500), "500 B");
        assert_eq!(format_filesize(1536), "1.5 KB");
        assert_eq!(format_filesize(1048576), "1.0 MB");
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

    // ── Extension ↔ content cross-check (audit finding H-4) ───────────────

    #[test]
    fn extension_match_accepts_aligned_filename_and_mime() {
        assert!(validate_filename_extension_matches("photo.png", "image/png").is_ok());
        assert!(validate_filename_extension_matches("doc.pdf", "application/pdf").is_ok());
    }

    #[test]
    fn extension_match_accepts_case_variations() {
        assert!(validate_filename_extension_matches("PHOTO.PNG", "image/png").is_ok());
        assert!(validate_filename_extension_matches("photo.JPEG", "image/jpeg").is_ok());
    }

    #[test]
    fn extension_match_rejects_html_posing_as_image() {
        // Core H-4 attack: attacker names a file `.html` while the content
        // is validated as PNG. If allowed, the file would later be served
        // as `text/html` and the PNG polyglot executed as a script.
        let err = validate_filename_extension_matches("evil.html", "image/png").unwrap_err();
        assert!(
            err.to_string().contains("Filename extension"),
            "expected extension-mismatch error, got: {err}",
        );
    }

    #[test]
    fn extension_match_rejects_svg_posing_as_image_png() {
        // SVG is still `image/*` but renders as HTML-adjacent content in
        // browsers. Strict mismatch check must catch this too.
        assert!(validate_filename_extension_matches("xss.svg", "image/png").is_err(),);
    }

    #[test]
    fn extension_match_accepts_filename_without_extension() {
        // No extension → served as octet-stream → no XSS surface.
        assert!(validate_filename_extension_matches("README", "text/plain").is_ok());
    }

    #[test]
    fn extension_match_accepts_leading_dot_dotfile() {
        // `.gitignore` has no "extension" in the XSS-relevant sense.
        assert!(validate_filename_extension_matches(".gitignore", "text/plain").is_ok());
    }

    #[test]
    fn extension_match_accepts_unknown_extension() {
        // Unknown extensions resolve to octet-stream via mime_guess —
        // served as a download, safe regardless of content.
        assert!(validate_filename_extension_matches("file.xyz123", "image/png").is_ok());
    }

    #[test]
    fn extension_match_allows_non_renderable_mismatch() {
        // `.txt` served as text/plain is never executed by the browser; a
        // content mismatch here is cosmetic, not a security issue. Clients
        // that ship files with claimed `application/octet-stream` should
        // not be blocked. Regression test for the process_upload fixture.
        assert!(
            validate_filename_extension_matches("notes.txt", "application/octet-stream").is_ok()
        );
        assert!(
            validate_filename_extension_matches("archive.zip", "application/octet-stream").is_ok()
        );
        assert!(validate_filename_extension_matches("photo.pdf", "image/png").is_ok());
    }

    #[test]
    fn extension_match_rejects_js_with_image_content() {
        assert!(validate_filename_extension_matches("xss.js", "image/png").is_err());
    }

    #[test]
    fn extension_match_allows_exact_renderable_match() {
        // A legitimate SVG served as image/svg+xml is fine.
        assert!(validate_filename_extension_matches("logo.svg", "image/svg+xml").is_ok(),);
    }
}
