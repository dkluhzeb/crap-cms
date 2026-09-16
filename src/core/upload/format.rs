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

impl FormatOptions {
    /// Every configured format paired with its options, in the order the upload
    /// pipeline produces them.
    fn configured(&self) -> impl Iterator<Item = (&'static str, &FormatQuality)> {
        [("webp", self.webp.as_ref()), ("avif", self.avif.as_ref())]
            .into_iter()
            .filter_map(|(name, opts)| Some((name, opts?)))
    }

    /// The formats configured to convert on the background queue instead of
    /// during the upload — the variants a stored file still owes.
    ///
    /// One rule for both sides of a deferred conversion: the upload pipeline
    /// defers them, and the publish that makes a drafted file live re-derives
    /// exactly the same set from the stored size columns.
    #[must_use]
    pub fn deferred(&self) -> Vec<(&'static str, &FormatQuality)> {
        self.configured().filter(|(_, opts)| opts.queue).collect()
    }
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

    /// Only a `queue = true` format is deferred; a synchronously converted one
    /// is produced during the upload and owes nothing to the queue.
    #[test]
    fn deferred_lists_only_the_queued_formats() {
        let opts = FormatOptions {
            webp: Some(FormatQuality::new(80, true)),
            avif: Some(FormatQuality::new(50, false)),
        };

        let deferred = opts.deferred();
        assert_eq!(deferred.len(), 1, "{deferred:?}");
        assert_eq!(deferred[0].0, "webp");
        assert_eq!(deferred[0].1.quality, 80);

        assert!(FormatOptions::default().deferred().is_empty());
    }

    #[test]
    fn format_result_new_sets_url() {
        assert_eq!(FormatResult::new("/uploads/x.webp").url, "/uploads/x.webp");
    }
}
