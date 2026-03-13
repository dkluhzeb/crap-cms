use serde::{Deserialize, Serialize};

use super::{format::FormatOptions, image_size::ImageSize};

/// Per-collection upload configuration (MIME filtering, image sizes, format options).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CollectionUpload {
    pub enabled: bool,
    #[serde(default)]
    pub mime_types: Vec<String>,
    #[serde(default)]
    pub max_file_size: Option<u64>,
    #[serde(default)]
    pub image_sizes: Vec<ImageSize>,
    #[serde(default)]
    pub admin_thumbnail: Option<String>,
    #[serde(default)]
    pub format_options: FormatOptions,
}

impl CollectionUpload {
    /// Create a new enabled upload config with defaults for all other fields.
    pub fn new() -> Self {
        Self {
            enabled: true,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_upload_default() {
        let upload = CollectionUpload::default();
        assert!(!upload.enabled);
        assert!(upload.mime_types.is_empty());
        assert!(upload.max_file_size.is_none());
        assert!(upload.image_sizes.is_empty());
        assert!(upload.admin_thumbnail.is_none());
        assert!(upload.format_options.webp.is_none());
        assert!(upload.format_options.avif.is_none());
    }
}
