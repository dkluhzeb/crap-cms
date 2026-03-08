use serde::{Deserialize, Serialize};

/// Optional format conversion settings (WebP and/or AVIF with quality).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FormatOptions {
    #[serde(default)]
    pub webp: Option<FormatQuality>,
    #[serde(default)]
    pub avif: Option<FormatQuality>,
}

/// Quality and processing settings for a converted image format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatQuality {
    pub quality: u8,
    /// When true, this format's conversion is deferred to the background image processing
    /// queue instead of happening synchronously during upload. Default: false.
    #[serde(default)]
    pub queue: bool,
}

impl FormatQuality {
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
