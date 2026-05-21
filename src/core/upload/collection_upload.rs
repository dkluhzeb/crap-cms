use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::core::upload::{FormatOptions, ImageSize};
use crate::typegen::lua::LuaAnnotation;

/// Per-collection upload configuration (MIME filtering, image sizes, format options).
#[derive(Debug, Clone, Default, Serialize, Deserialize, LuaAnnotation)]
#[lua(class = "crap.CollectionUpload")]
pub struct CollectionUpload {
    // Internal toggle: set automatically when the user provides `upload = true`
    // or `upload = { ... }` at the collection level. Not exposed in the Lua
    // API (the higher-level shorthand makes this implicit).
    #[lua(skip)]
    pub enabled: bool,
    /// MIME type allowlist with glob support (e.g., "image/*"). Empty = any type.
    #[serde(default)]
    #[lua(optional)]
    pub mime_types: Vec<String>,
    /// Max file size — bytes (integer) or human-readable ("10MB", "1GB"). Overrides global default.
    #[serde(default)]
    #[lua(ty = "integer|string")]
    pub max_file_size: Option<u64>,
    /// Resize definitions for image uploads.
    #[serde(default)]
    #[lua(optional, ty = "crap.ImageSize[]")]
    pub image_sizes: Vec<ImageSize>,
    /// Name of `image_size` to show in admin list.
    #[serde(default)]
    pub admin_thumbnail: Option<String>,
    /// Auto-generate format variants for each size.
    #[serde(default)]
    #[lua(optional)]
    pub format_options: FormatOptions,
}

impl CollectionUpload {
    /// Create a new enabled upload config with defaults for all other fields.
    #[must_use]
    pub fn new() -> Self {
        Self {
            enabled: true,
            ..Default::default()
        }
    }

    /// Return the set of system-injected field names that are auto-populated
    /// by the upload processing system (not user input).
    /// Mirrors the fields created by `inject_upload_fields()` in the Lua parser.
    #[must_use]
    pub fn system_field_names(&self) -> HashSet<String> {
        let mut names: HashSet<String> = [
            "filename",
            "mime_type",
            "filesize",
            "width",
            "height",
            "url",
            "focal_x",
            "focal_y",
        ]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();

        for size in &self.image_sizes {
            names.insert(format!("{}_url", size.name));
            names.insert(format!("{}_width", size.name));
            names.insert(format!("{}_height", size.name));

            if self.format_options.webp.is_some() {
                names.insert(format!("{}_webp_url", size.name));
            }
            if self.format_options.avif.is_some() {
                names.insert(format!("{}_avif_url", size.name));
            }
        }

        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::upload::{FormatQuality, ImageSizeBuilder};

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

    #[test]
    fn system_field_names_base() {
        let upload = CollectionUpload::new();
        let names = upload.system_field_names();
        assert!(names.contains("filename"));
        assert!(names.contains("mime_type"));
        assert!(names.contains("filesize"));
        assert!(names.contains("width"));
        assert!(names.contains("height"));
        assert!(names.contains("url"));
        assert!(names.contains("focal_x"));
        assert!(names.contains("focal_y"));
        assert_eq!(names.len(), 8);
    }

    #[test]
    fn system_field_names_with_sizes_and_formats() {
        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumb")
                .width(300)
                .height(300)
                .build(),
        ];
        upload.format_options.webp = Some(FormatQuality::new(80, false));
        upload.format_options.avif = Some(FormatQuality::new(60, true));

        let names = upload.system_field_names();
        // 8 base + 3 per-size + 2 format variants
        assert_eq!(names.len(), 13);
        assert!(names.contains("thumb_url"));
        assert!(names.contains("thumb_width"));
        assert!(names.contains("thumb_height"));
        assert!(names.contains("thumb_webp_url"));
        assert!(names.contains("thumb_avif_url"));
    }
}
