use serde::{Deserialize, Serialize};

use crate::typegen::lua::LuaAnnotation;

/// Auto-generate format variants for each upload size (e.g. WebP, AVIF).
#[derive(Debug, Clone, Serialize, Deserialize, Default, LuaAnnotation)]
#[lua(class = "crap.FormatOptions")]
pub struct FormatOptions {
    /// Auto-generate WebP variant for each size.
    #[serde(default)]
    pub webp: Option<FormatQuality>,
    /// Auto-generate AVIF variant for each size.
    #[serde(default)]
    pub avif: Option<FormatQuality>,
}

/// Encoding quality and processing mode for a single converted image format.
#[derive(Debug, Clone, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.FormatQuality")]
pub struct FormatQuality {
    /// Encoding quality 1-100.
    pub quality: u8,
    /// Defer conversion to the background image-processing queue
    /// instead of running synchronously during upload (default: false).
    #[serde(default)]
    #[lua(optional)]
    pub queue: bool,
}

impl FormatQuality {
    #[must_use]
    pub fn new(quality: u8, queue: bool) -> Self {
        Self { quality, queue }
    }
}

/// Output metadata for a single converted format variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatResult {
    pub url: String,
}

impl FormatResult {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_quality_new_and_serde_default_queue() {
        let fq = FormatQuality::new(80, true);
        assert_eq!(fq.quality, 80);
        assert!(fq.queue);

        // `queue` defaults to false when omitted from config.
        let parsed: FormatQuality = serde_json::from_value(json!({ "quality": 75 })).unwrap();
        assert_eq!(parsed.quality, 75);
        assert!(!parsed.queue);
    }

    #[test]
    fn format_options_default_and_empty_config_have_no_variants() {
        let opts = FormatOptions::default();
        assert!(opts.webp.is_none() && opts.avif.is_none());

        let parsed: FormatOptions = serde_json::from_value(json!({})).unwrap();
        assert!(parsed.webp.is_none() && parsed.avif.is_none());
    }

    #[test]
    fn format_result_new_sets_url() {
        assert_eq!(FormatResult::new("/uploads/x.webp").url, "/uploads/x.webp");
    }
}
