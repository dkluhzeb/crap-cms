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
                .map(std::string::ToString::to_string)
        })
        .or_else(|| doc.get_str("url").map(std::string::ToString::to_string))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::{Value, json};

    use crate::core::document::DocumentBuilder;

    use super::*;

    fn doc(fields: Value) -> Document {
        let map: HashMap<String, Value> = serde_json::from_value(fields).unwrap();
        DocumentBuilder::new("u1").fields(map).build()
    }

    #[test]
    fn non_image_mime_yields_no_thumbnail() {
        let d = doc(json!({ "mime_type": "application/pdf", "url": "/f.pdf" }));
        assert_eq!(thumbnail_url(&d, Some("card")), None);
    }

    #[test]
    fn image_uses_configured_size_url_when_present() {
        let d = doc(json!({
            "mime_type": "image/png",
            "url": "/orig.png",
            "sizes": { "card": { "url": "/card.png" } }
        }));
        assert_eq!(
            thumbnail_url(&d, Some("card")).as_deref(),
            Some("/card.png")
        );
    }

    #[test]
    fn image_falls_back_to_original_when_size_missing() {
        let d = doc(json!({
            "mime_type": "image/png",
            "url": "/orig.png",
            "sizes": { "other": { "url": "/other.png" } }
        }));
        assert_eq!(
            thumbnail_url(&d, Some("card")).as_deref(),
            Some("/orig.png")
        );
    }

    #[test]
    fn image_without_thumbnail_config_uses_original() {
        let d = doc(json!({ "mime_type": "image/jpeg", "url": "/orig.jpg" }));
        assert_eq!(thumbnail_url(&d, None).as_deref(), Some("/orig.jpg"));
    }

    #[test]
    fn image_with_no_url_at_all_yields_none() {
        let d = doc(json!({ "mime_type": "image/png" }));
        assert_eq!(thumbnail_url(&d, Some("card")), None);
    }
}
