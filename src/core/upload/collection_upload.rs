use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::core::{
    FieldType,
    upload::{FormatOptions, ImageSize, SIZES_FIELD},
};
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

    /// The largest file this collection accepts: its own `max_file_size`, or
    /// `global` (the `[upload] max_file_size` default) when it sets none.
    #[must_use]
    pub fn max_file_size_or(&self, global: u64) -> u64 {
        self.max_file_size.unwrap_or(global)
    }

    /// The configured format-variant names, in the order their columns are
    /// generated. The one place `webp`/`avif` are spelled as wire names.
    #[must_use]
    pub fn format_variants(&self) -> Vec<&'static str> {
        let mut variants = Vec::new();

        if self.format_options.webp.is_some() {
            variants.push("webp");
        }
        if self.format_options.avif.is_some() {
            variants.push("avif");
        }

        variants
    }

    /// Every per-size column the schema injection generates, paired with the
    /// type its column holds, in injection order: per image size `{name}_url`,
    /// `{name}_width`, `{name}_height`, then `{name}_{format}_url` for each
    /// configured format variant.
    ///
    /// The single place these names are spelled — the schema injection, the
    /// system/derived field sets, and the read shape that folds them into the
    /// nested `sizes` object all derive from it, so a new per-size column lands
    /// on every one of them at once.
    #[must_use]
    pub fn size_columns(&self) -> Vec<(String, FieldType)> {
        let mut columns = Vec::new();

        for size in &self.image_sizes {
            columns.push((format!("{}_url", size.name), FieldType::Text));
            columns.push((format!("{}_width", size.name), FieldType::Number));
            columns.push((format!("{}_height", size.name), FieldType::Number));

            for format in self.format_variants() {
                columns.push((format!("{}_{format}_url", size.name), FieldType::Text));
            }
        }

        columns
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

        names.extend(self.size_columns().into_iter().map(|(name, _)| name));

        names
    }

    /// The field names a user schema may not define on this upload collection:
    /// the injected columns, plus the `sizes` key a read assembles from the
    /// per-size columns. A user field named `sizes` would be overwritten on
    /// every read, so it is rejected at load instead.
    #[must_use]
    pub fn reserved_field_names(&self) -> HashSet<String> {
        let mut names = self.system_field_names();

        if !self.image_sizes.is_empty() {
            names.insert(SIZES_FIELD.to_string());
        }

        names
    }

    /// The server-**derived** subset of the system fields: exactly the columns
    /// `inject_upload_metadata` computes from the processed file (`filename`,
    /// `mime_type`, `filesize`, `width`, `height`, `url`, and every
    /// `{size}[_fmt]_url` / `{size}_width` / `{size}_height`). These must never
    /// be settable from user input — the serve access gate matches a request
    /// against the stored `url`/`*_url` columns and `delete_upload_files` uses
    /// them as deletion targets, so a user-forged value there defeats the
    /// per-document read gate and can delete another document's file. The write
    /// chokepoint strips these from untrusted input.
    ///
    /// `focal_x` / `focal_y` are deliberately excluded: the focal point is a
    /// legitimate user-editable setting, not derived from the file.
    #[must_use]
    pub fn derived_field_names(&self) -> HashSet<String> {
        let mut names = self.system_field_names();
        names.remove("focal_x");
        names.remove("focal_y");

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
    fn max_file_size_or_prefers_the_collection_limit() {
        let mut upload = CollectionUpload::new();
        assert_eq!(upload.max_file_size_or(100), 100);

        upload.max_file_size = Some(500);
        assert_eq!(upload.max_file_size_or(100), 500);
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

    /// The derived set is the file-computed columns only — every URL-bearing and
    /// dimension column the serve gate / cleanup path trust — and must EXCLUDE
    /// the user-editable focal point, or focal-point edits would be silently
    /// dropped by the write chokepoint's strip.
    #[test]
    fn derived_field_names_excludes_focal_but_keeps_url_bearing() {
        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumb")
                .width(300)
                .height(300)
                .build(),
        ];
        upload.format_options.avif = Some(FormatQuality::new(60, true));

        let derived = upload.derived_field_names();

        // Focal point is user-editable, never derived from the file.
        assert!(!derived.contains("focal_x"));
        assert!(!derived.contains("focal_y"));

        // Everything the serve gate / delete path trusts must be locked.
        for name in [
            "filename",
            "mime_type",
            "filesize",
            "width",
            "height",
            "url",
        ] {
            assert!(derived.contains(name), "derived set missing {name}");
        }
        assert!(derived.contains("thumb_url"));
        assert!(derived.contains("thumb_avif_url"));
        assert!(derived.contains("thumb_width"));

        // Exactly system_field_names minus the two focal columns.
        assert_eq!(derived.len(), upload.system_field_names().len() - 2);
    }

    /// The per-size columns are generated in injection order and carry the
    /// column type each holds — the schema injection types its fields from
    /// this, so a URL column must never be typed as a number.
    #[test]
    fn size_columns_list_every_column_in_injection_order() {
        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumb")
                .width(300)
                .height(300)
                .build(),
        ];
        upload.format_options.webp = Some(FormatQuality::new(80, false));
        upload.format_options.avif = Some(FormatQuality::new(60, false));

        let columns = upload.size_columns();
        let names: Vec<&str> = columns.iter().map(|(n, _)| n.as_str()).collect();

        assert_eq!(
            names,
            [
                "thumb_url",
                "thumb_width",
                "thumb_height",
                "thumb_webp_url",
                "thumb_avif_url",
            ]
        );
        assert_eq!(columns[0].1, FieldType::Text);
        assert_eq!(columns[1].1, FieldType::Number);
        assert_eq!(columns[2].1, FieldType::Number);
        assert_eq!(columns[3].1, FieldType::Text);
    }

    #[test]
    fn format_variants_follow_the_configured_options() {
        let mut upload = CollectionUpload::new();
        assert!(upload.format_variants().is_empty());

        upload.format_options.avif = Some(FormatQuality::new(60, false));
        assert_eq!(upload.format_variants(), ["avif"]);

        upload.format_options.webp = Some(FormatQuality::new(80, false));
        assert_eq!(upload.format_variants(), ["webp", "avif"]);
    }

    /// `sizes` is the key a read assembles, so a user field of that name is
    /// reserved — but only where per-size columns exist to assemble it from.
    #[test]
    fn reserved_names_add_sizes_only_when_image_sizes_exist() {
        let plain = CollectionUpload::new();
        assert!(!plain.reserved_field_names().contains("sizes"));
        assert_eq!(
            plain.reserved_field_names().len(),
            plain.system_field_names().len()
        );

        let mut sized = CollectionUpload::new();
        sized.image_sizes = vec![
            ImageSizeBuilder::new("thumb")
                .width(300)
                .height(300)
                .build(),
        ];

        assert!(sized.reserved_field_names().contains("sizes"));
        assert!(sized.reserved_field_names().contains("thumb_url"));
    }
}
