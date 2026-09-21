use std::{io::Cursor, str, sync::LazyLock};

use anyhow::{Context as _, Result, bail};
use image::{ImageFormat, ImageReader};
use regex::{Captures, Regex};

use crate::core::upload::{CollectionUpload, UploadedFile};

/// Longest sanitised filename an upload may carry.
///
/// What reaches the filesystem is longer than what the user typed: the write
/// path stores `{id}_{sanitized}` (11 characters of prefix) and a resized
/// variant inserts `_{size_name}` before the extension. A single path
/// component is capped at 255 bytes on every filesystem we target, so this
/// leaves room for the prefix plus a size name of up to 40 characters.
/// Without the cap an over-long name reaches the local backend and fails with
/// a raw OS error instead of a message the uploader can act on.
const MAX_SANITIZED_FILENAME_LEN: usize = 200;

/// Reject a filename that cannot fit the stored-name budget.
fn validate_filename_length(filename: &str) -> Result<()> {
    let length = sanitize_filename(filename).len();

    if length <= MAX_SANITIZED_FILENAME_LEN {
        return Ok(());
    }

    bail!(
        "File name is too long ({length} bytes once sanitized, maximum \
         {MAX_SANITIZED_FILENAME_LEN}). Rename the file before uploading.",
    );
}

/// Validate MIME type, magic bytes, and file size of an uploaded file.
pub(super) fn validate_upload(
    file: &UploadedFile,
    upload_config: &CollectionUpload,
    global_max_file_size: u64,
) -> Result<()> {
    validate_filename_length(&file.filename)?;

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

    // SVG-specific: reject XXE / external-entity vectors. SVGs are served
    // with `Content-Disposition: attachment` and a sandbox CSP today, but a
    // future code path (thumbnailing, rasterisation, inline rendering) may
    // parse them server- or client-side, where `<!DOCTYPE>` / `<!ENTITY>`
    // declarations or external `xlink:href` loads could leak data. Scan
    // once at upload so only clean SVGs ever land in storage.
    if is_svg(&effective_mime, &file.data) {
        validate_svg_content(&file.data)?;
    }

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
        "Filename extension implies renderable type '{ext_mime_str}' but content is '{effective_mime}' — \
         rename the file with an extension that matches its actual type",
    );
}

/// Whether this build can actually decode `content_type` — the condition for
/// running the pixel pipeline (bomb check, dimensions, resize, format
/// variants) over a file.
///
/// "Starts with `image/`" is not that condition. SVG has no raster decoder at
/// all, and AVIF decoding needs `image`'s `avif-native` feature (dav1d) while
/// the enabled `avif` feature only builds the *encoder* — so
/// `ImageFormat::reading_enabled` reports AVIF readable when it is not.
/// A content type that lands here as undecodable is stored verbatim instead
/// of being rejected for a decode that was never going to run.
pub(super) fn decodable_image(content_type: &str) -> bool {
    let Some(format) = ImageFormat::from_mime_type(content_type) else {
        return false;
    };

    if format == ImageFormat::Avif {
        return false;
    }

    format.reading_enabled()
}

/// Check image dimensions against the decompression bomb limit.
///
/// Two guards run:
/// 1. Absolute pixel cap (100 MP) — rejects e.g. a 20k×20k image that
///    would allocate ~1.6 GB of RGBA during decode.
/// 2. Pixel-to-byte ratio cap — rejects the class of "small file, huge
///    declared dimensions" attacks where a tightly-compressed payload
///    expands absurdly during decode even though its file size is tiny.
///    Threshold is 500 pixels per byte: a 10 kB file is capped at 5 MP,
///    a 1 MB file can declare up to 500 MP (also caught by guard 1). Real
///    photographs sit in the single-digit range, so normal uploads pass.
pub(super) fn check_image_dimensions(data: &[u8]) -> Result<()> {
    const MAX_PIXELS: u64 = 100_000_000;
    const MAX_PIXELS_PER_BYTE: u64 = 500;

    let reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .context("Failed to detect image format")?;

    // Fail closed if the header dimensions can't be read: skipping the checks
    // and proceeding to a full `image::load_from_memory` decode is exactly what
    // the bomb guard exists to prevent. A genuinely malformed image will be
    // rejected here rather than during an unbounded decode.
    let (w, h) = reader
        .into_dimensions()
        .context("Failed to read image dimensions for the decompression-bomb check")?;

    let pixels = u64::from(w) * u64::from(h);

    if pixels > MAX_PIXELS {
        bail!("Image too large: {w}x{h} exceeds pixel limit");
    }

    // `data.len() + 1` prevents a pathological zero-byte file (rare but possible
    // via header-only streams) from producing division by zero; zero-byte inputs
    // would have already failed to decode.
    let ratio = pixels / (data.len() as u64 + 1);

    if ratio > MAX_PIXELS_PER_BYTE {
        bail!(
            "Image compression ratio too high: {}x{} pixels in {} bytes \
             (ratio {} > {}). Likely a decompression bomb.",
            w,
            h,
            data.len(),
            ratio,
            MAX_PIXELS_PER_BYTE,
        );
    }

    Ok(())
}

/// Best-effort check whether an uploaded file is an SVG. `infer` does not
/// classify text-based formats, so we also peek at the raw bytes.
fn is_svg(effective_mime: &str, data: &[u8]) -> bool {
    if effective_mime == "image/svg+xml" {
        return true;
    }

    // Look only at the first 1 kB — enough to find the XML / <svg> prolog
    // without paying for a full scan on non-SVG content.
    let head = &data[..data.len().min(1024)];
    let prefix = str::from_utf8(head).unwrap_or("");
    let trimmed = prefix.trim_start();
    let lower = trimmed.to_ascii_lowercase();

    lower.starts_with("<?xml") && lower.contains("<svg") || lower.starts_with("<svg")
}

/// Reject SVGs that carry the classic XXE indicators — a DOCTYPE
/// declaration (gateway to external/general entity abuse) or an explicit
/// ENTITY declaration — or an embedded `<script>` element. Case-insensitive
/// because XML is case-sensitive-but-tags-are-conventionally-lowercase and
/// the attack strings are well-known ASCII tokens.
/// Every quoted `href` / `xlink:href` attribute value (group 1), and every
/// `url(…)` argument in a style or paint attribute (group 2) — the two places
/// an SVG names something for the renderer to fetch.
static REFERENCE_VALUES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:(?:xlink:)?href\s*=\s*["']([^"']*)["'])|(?:url\(\s*["']?([^"')]*))"#)
        .expect("static regex")
});

/// A value that starts with a URL scheme or a protocol-relative `//`; group 1
/// is the scheme. `data:` is the only scheme an uploaded asset legitimately
/// embeds.
static LEADING_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?://|([a-z][a-z0-9+.\-]*):)").expect("static regex"));

/// An inline event handler (`onload=`, `onclick=`, …) runs script without a
/// `<script>` element.
/// Numeric character references plus the named ones that can spell a scheme.
static CHARACTER_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)&(?:#x([0-9a-f]+)|#([0-9]+)|(colon|sol|tab|newline));").expect("static regex")
});

static EVENT_HANDLER_ATTRIBUTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)[\s"'/]on[a-z]+\s*="#).expect("static regex"));

/// The value as a renderer reads it: numeric and the scheme-relevant named
/// character references decoded, ASCII controls and whitespace removed —
/// browsers strip those inside a URL, so `java&#9;script:` is `javascript:`.
fn normalized_reference(raw: &str) -> String {
    let decoded = CHARACTER_REFERENCE.replace_all(raw, |caps: &Captures<'_>| {
        let code = caps
            .get(1)
            .and_then(|m| u32::from_str_radix(m.as_str(), 16).ok())
            .or_else(|| caps.get(2).and_then(|m| m.as_str().parse().ok()))
            .or_else(
                || match caps.get(3).map(|m| m.as_str().to_ascii_lowercase()) {
                    Some(name) if name == "colon" => Some(u32::from(':')),
                    Some(name) if name == "sol" => Some(u32::from('/')),
                    Some(_) => Some(u32::from(' ')),
                    None => None,
                },
            );

        code.and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_default()
    });

    decoded
        .chars()
        .filter(|c| !c.is_ascii_control() && !c.is_whitespace())
        .collect()
}

/// A reference that makes a renderer fetch (or execute) something outside the
/// file: any scheme except `data:`, or a protocol-relative URL. Fragments,
/// relative paths and `data:` URIs stay inside the file.
fn external_reference(text: &str) -> Option<String> {
    REFERENCE_VALUES.captures_iter(text).find_map(|caps| {
        let raw = caps
            .get(1)
            .or_else(|| caps.get(2))
            .map_or("", |m| m.as_str());
        let value = normalized_reference(raw).to_ascii_lowercase();
        let scheme = LEADING_SCHEME.captures(&value)?;
        let is_data = scheme.get(1).is_some_and(|m| m.as_str() == "data");

        (!is_data).then(|| raw.chars().take(60).collect())
    })
}

fn validate_svg_content(data: &[u8]) -> Result<()> {
    let text = str::from_utf8(data).context("SVG is not valid UTF-8")?;
    let lower = text.to_ascii_lowercase();

    // A remote reference turns every render into a request the uploader chose:
    // a tracking beacon at best, `javascript:` at worst. Fragments, relative
    // paths and `data:` URIs stay inside the file and are allowed; `mailto:`
    // and `tel:` links are refused with the rest — an uploaded image is a
    // static asset, not a document with links.
    if let Some(reference) = external_reference(text) {
        bail!(
            "SVG references an external resource ({reference}…). Remove it — \
             uploaded images must not load or execute anything outside the file \
             (only fragments, relative paths and data: URIs are allowed)."
        );
    }

    if EVENT_HANDLER_ATTRIBUTE.is_match(text) {
        bail!(
            "SVG contains an inline event handler (on…= attribute). Remove it — \
             uploaded images are static assets and must not run script."
        );
    }

    if lower.contains("@import") {
        bail!(
            "SVG contains a CSS @import. Remove it — uploaded images must not \
             load anything outside the file."
        );
    }

    // An SVG is served as a download under a sandbox CSP, so a script inside
    // one cannot run from the serve route. It would still run if the file is
    // ever opened directly from disk or re-served by a consumer that does not
    // reproduce those headers, and no legitimate uploaded asset needs one.
    if lower.contains("<script") {
        bail!(
            "SVG contains a <script> element. Remove it — uploaded images are \
             static assets, and a scripted SVG is a stored-XSS vector for any \
             consumer that renders it inline."
        );
    }

    if lower.contains("<!doctype") {
        bail!(
            "SVG contains a <!DOCTYPE> declaration. Remove it — DOCTYPE is \
             an XXE gateway and not required for SVGs that render in any \
             modern browser."
        );
    }

    if lower.contains("<!entity") {
        bail!("SVG contains an <!ENTITY> declaration — reject as a potential XXE vector.");
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
    // Extension: ASCII alphanumerics only. Anything else ("?", "&", "#",
    // "%", CRLF, …) is dropped — a crafted extension used to survive
    // verbatim, breaking every consumer that embeds the stored filename in
    // a URL (signed upload URLs would mint a self-truncating link) or a
    // header (Content-Disposition keeps its own belt-and-braces guard for
    // files stored by older versions).
    let clean_ext: Option<String> = ext
        .map(|e| {
            e.chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
        })
        .filter(|e| !e.is_empty());

    match clean_ext {
        Some(e) => format!("{clean_stem}.{e}"),
        None => clean_stem,
    }
}

/// Format a file size in human-readable form.
/// Format a byte count as a human-readable string. Integer math avoids the
/// `u64 as f64` precision-loss path.
#[must_use]
pub fn format_filesize(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    fn split(bytes: u64, unit: u64) -> (u64, u64) {
        (bytes / unit, (bytes % unit) * 10 / unit)
    }

    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        let (whole, tenths) = split(bytes, KB);
        format!("{whole}.{tenths} KB")
    } else if bytes < GB {
        let (whole, tenths) = split(bytes, MB);
        format!("{whole}.{tenths} MB")
    } else {
        let (whole, tenths) = split(bytes, GB);
        format!("{whole}.{tenths} GB")
    }
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
    use super::*;
    use crate::core::upload::STORED_ID_LEN;

    /// Regression: dimensions that can't be read must FAIL the bomb check, not
    /// fall through to a full decode. A valid PNG signature with no IHDR is
    /// format-detectable but dimensionless.
    #[test]
    fn image_dimensions_fail_closed_when_unreadable() {
        let png_sig = [0x89u8, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        assert!(
            check_image_dimensions(&png_sig).is_err(),
            "unreadable dimensions must be rejected, not passed through to decode"
        );
    }

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

    /// Regression: the extension used to survive verbatim — a crafted
    /// upload could smuggle "?", "&", "#" or CRLF into the stored filename,
    /// silently breaking signed upload URLs minted from it (the link
    /// self-truncates at the "?") and relying on the Content-Disposition
    /// guard for header safety.
    #[test]
    fn sanitize_filename_extension_charset() {
        assert_eq!(sanitize_filename("report.pd?f"), "report.pdf");
        assert_eq!(sanitize_filename("photo.j\r\npg"), "photo.jpg");
        assert_eq!(sanitize_filename("x.a&b#c"), "x.abc");
        assert_eq!(sanitize_filename("evil.???"), "evil");
        assert_eq!(sanitize_filename("archive.tar.gz"), "archive-tar.gz");
    }

    /// Regression: an over-long name used to travel all the way to the local
    /// backend, which failed with a raw OS error. It is refused at the upload
    /// boundary now, with room left for the id prefix and a size suffix.
    #[test]
    fn an_over_long_filename_is_refused_with_a_clear_message() {
        let long = format!("{}.png", "a".repeat(MAX_SANITIZED_FILENAME_LEN));

        let err = validate_filename_length(&long).unwrap_err().to_string();

        assert!(err.contains("too long"), "{err}");
        assert!(
            err.contains(&MAX_SANITIZED_FILENAME_LEN.to_string()),
            "{err}"
        );
    }

    #[test]
    fn a_name_within_the_budget_is_accepted() {
        let at_cap = format!("{}.png", "a".repeat(MAX_SANITIZED_FILENAME_LEN - 4));

        assert_eq!(sanitize_filename(&at_cap).len(), MAX_SANITIZED_FILENAME_LEN);
        assert!(validate_filename_length(&at_cap).is_ok());
        assert!(validate_filename_length("holiday-photo.jpg").is_ok());
    }

    /// The cap leaves room for what the write path adds: the id prefix and a
    /// resized variant's `_{size}` suffix must still fit one path component.
    #[test]
    fn the_cap_leaves_room_for_the_stored_name_and_a_size_suffix() {
        const MAX_PATH_COMPONENT: usize = 255;

        // `{id}_{sanitized}`, then `_{size_name}` for a resized variant.
        let stored = STORED_ID_LEN + 1 + MAX_SANITIZED_FILENAME_LEN;
        let with_suffix = stored + 1 + 40;

        assert!(with_suffix <= MAX_PATH_COMPONENT, "{with_suffix}");
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

    // ── SVG XXE scan (audit finding M-5) ─────────────────────────────────

    #[test]
    fn is_svg_recognises_svg_mime() {
        assert!(is_svg("image/svg+xml", b""));
    }

    #[test]
    fn is_svg_recognises_raw_svg_prolog() {
        assert!(is_svg(
            "application/octet-stream",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        ));
        assert!(is_svg(
            "application/octet-stream",
            b"<?xml version=\"1.0\"?>\n<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        ));
    }

    #[test]
    fn is_svg_rejects_non_svg_content() {
        assert!(!is_svg("image/png", &[0x89, 0x50, 0x4E, 0x47]));
        assert!(!is_svg("text/html", b"<html><body></body></html>"));
    }

    #[test]
    fn svg_scan_accepts_clean_svg() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <rect width="10" height="10" fill="red"/>
        </svg>"#;
        assert!(validate_svg_content(svg).is_ok());
    }

    /// Regression: `image/*` was taken to mean "decodable", so an SVG (no
    /// raster decoder) and an AVIF (encode-only build) were rejected with
    /// "Failed to detect image format" instead of being stored.
    #[test]
    fn decodable_image_covers_only_types_with_an_enabled_decoder() {
        assert!(decodable_image("image/png"));
        assert!(decodable_image("image/jpeg"));
        assert!(decodable_image("image/gif"));
        assert!(decodable_image("image/webp"));

        assert!(!decodable_image("image/svg+xml"), "SVG has no decoder");
        assert!(!decodable_image("image/avif"), "avif encodes only");
        assert!(!decodable_image("image/bmp"), "BMP is not enabled");
        assert!(!decodable_image("application/pdf"));
        assert!(!decodable_image("text/plain"));
        assert!(!decodable_image(""));
    }

    /// Entity-encoded or whitespace-split schemes are what a renderer sees
    /// after decoding, so the scan judges the decoded value.
    #[test]
    fn svg_scan_decodes_the_reference_before_judging_it() {
        for href in [
            "&#x6a;avascript:alert(1)",
            "&#104;ttps://evil.example/x.png",
            "java&#9;script:alert(1)",
            "javascript&colon;alert(1)",
            " \n https://evil.example/x.png",
        ] {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg"><a href="{href}"><text>x</text></a></svg>"#
            );
            assert!(
                validate_svg_content(svg.as_bytes()).is_err(),
                "{href} must be rejected"
            );
        }
    }

    /// Script runs from an event-handler attribute without any `<script>`
    /// element, and a stylesheet import or a `url(…)` paint reference fetches
    /// from outside the file.
    #[test]
    fn svg_scan_rejects_event_handlers_and_style_fetches() {
        let handler = br#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"/>"#;
        assert!(validate_svg_content(handler).is_err());

        let import = br#"<svg xmlns="http://www.w3.org/2000/svg"><style>@import url(https://evil.example/a.css);</style></svg>"#;
        assert!(validate_svg_content(import).is_err());

        let paint = br#"<svg xmlns="http://www.w3.org/2000/svg"><rect fill="url(https://evil.example/p.svg#g)"/></svg>"#;
        assert!(validate_svg_content(paint).is_err());

        let local_paint = br#"<svg xmlns="http://www.w3.org/2000/svg"><rect fill="url(#grad)" font-family="Onyx"/></svg>"#;
        assert!(validate_svg_content(local_paint).is_ok());
    }

    #[test]
    fn svg_scan_rejects_external_href() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><image xlink:href="https://evil.example/pixel.png"/></svg>"#;
        let err = validate_svg_content(svg).expect_err("remote image must be rejected");
        assert!(err.to_string().contains("external resource"), "{err}");

        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><a href="javascript:alert(1)"><text>x</text></a></svg>"#;
        assert!(validate_svg_content(svg).is_err());

        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"><image href = '//cdn.example/x.png'/></svg>"#;
        assert!(validate_svg_content(svg).is_err());
    }

    /// The `xmlns:xlink` declaration, fragment references and embedded
    /// `data:` images are the legitimate shapes and must keep passing.
    #[test]
    fn svg_scan_accepts_internal_and_data_hrefs() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink"><defs><circle id="c" r="1"/></defs><use xlink:href="#c"/><use href="#c"/><image href="data:image/png;base64,iVBORw0KGgo="/><image href="logo.png"/></svg>"##;
        assert!(validate_svg_content(svg).is_ok());
    }

    #[test]
    fn svg_scan_rejects_script_element() {
        let payload = br#"<svg xmlns="http://www.w3.org/2000/svg">
            <script>alert(document.domain)</script>
        </svg>"#;
        let err = validate_svg_content(payload).unwrap_err().to_string();
        assert!(err.contains("<script>"), "unexpected error: {err}");
    }

    #[test]
    fn svg_scan_rejects_script_element_in_any_case() {
        let payload = b"<svg><SCRIPT type=\"text/javascript\">x()</SCRIPT></svg>";
        assert!(validate_svg_content(payload).is_err());
    }

    #[test]
    fn svg_scan_rejects_doctype() {
        let payload = br#"<?xml version="1.0"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "svg11.dtd">
<svg xmlns="http://www.w3.org/2000/svg"/>"#;
        let err = validate_svg_content(payload).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("doctype"));
    }

    #[test]
    fn svg_scan_rejects_classic_xxe_payload() {
        // Textbook SVG XXE: DOCTYPE + ENTITY + use-of-entity to
        // exfiltrate a local file through a text node.
        let payload = br#"<?xml version="1.0"?>
<!DOCTYPE svg [
  <!ENTITY xxe SYSTEM "file:///etc/passwd">
]>
<svg xmlns="http://www.w3.org/2000/svg"><text>&xxe;</text></svg>"#;
        assert!(validate_svg_content(payload).is_err());
    }

    #[test]
    fn svg_scan_rejects_entity_even_without_doctype() {
        // Some XML parsers accept inline entity declarations even without
        // a DOCTYPE. Belt-and-braces: catch both markers independently.
        let payload = br#"<svg xmlns="http://www.w3.org/2000/svg">
            <!ENTITY evil SYSTEM "http://attacker.example/beacon"/>
        </svg>"#;
        assert!(validate_svg_content(payload).is_err());
    }

    #[test]
    fn svg_scan_is_case_insensitive() {
        // Attackers can vary case to try to bypass a naive scan. Reject
        // the lowercase form too.
        let payload = b"<!doctype svg><svg/>";
        assert!(validate_svg_content(payload).is_err());
    }

    // ── Image decompression ratio (audit finding M-7) ────────────────────
    //
    // The concrete decode path uses `image::ImageReader`, which needs real
    // format bytes to parse. Rather than crafting a PNG bomb fixture here,
    // we cover the threshold arithmetic directly — the ratio path is
    // exercised end-to-end by the `process_upload_image_*` tests in
    // `process.rs`.

    #[test]
    fn decompression_ratio_threshold_catches_obvious_bomb() {
        // 10 kB file claiming 20 000 × 20 000 = 400 MP. Ratio = 40 000.
        let pixels: u64 = 20_000 * 20_000;
        let bytes: u64 = 10_000;
        assert!(pixels / (bytes + 1) > 500);
    }

    #[test]
    fn decompression_ratio_threshold_allows_normal_photo() {
        // 4032 × 3024 JPEG (typical phone photo), ~2 MB file. Ratio ≈ 6.
        let pixels: u64 = 4032 * 3024;
        let bytes: u64 = 2 * 1024 * 1024;
        assert!(pixels / (bytes + 1) < 500);
    }
}
