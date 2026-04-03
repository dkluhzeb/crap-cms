//! Image/thumbnail helpers for upload collections.

use crate::core::Document;

/// Extract the thumbnail URL for an upload document, if it's an image.
///
/// Prefers the admin thumbnail size if configured, falls back to the original URL.
pub(in crate::admin::handlers::collections) fn thumbnail_url(
    doc: &Document,
    admin_thumbnail: Option<&str>,
) -> Option<String> {
    let mime = doc.get_str("mime_type").unwrap_or("");

    if !mime.starts_with("image/") {
        return None;
    }

    admin_thumbnail
        .and_then(|thumb_name| {
            doc.fields
                .get("sizes")
                .and_then(|v| v.get(thumb_name))
                .and_then(|v| v.get("url"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .or_else(|| doc.get_str("url").map(|s| s.to_string()))
}
