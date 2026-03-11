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
}
