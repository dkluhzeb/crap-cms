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
