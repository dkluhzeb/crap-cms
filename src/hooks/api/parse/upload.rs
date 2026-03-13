//! Parsing functions for collection upload configuration.

use mlua::{Table, Value};

use crate::core::field::{FieldAdmin, FieldDefinition, FieldType};
use crate::core::upload::ImageSizeBuilder;
use crate::core::upload::{CollectionUpload, FormatOptions, FormatQuality, ImageFit, ImageSize};

use super::helpers::*;

pub(super) fn parse_collection_upload(config: &Table) -> Option<CollectionUpload> {
    let val: Value = config.get("upload").ok()?;
    match val {
        Value::Boolean(true) => Some(CollectionUpload::new()),
        Value::Boolean(false) | Value::Nil => None,
        Value::Table(tbl) => {
            let mime_types = if let Ok(mt_tbl) = get_table(&tbl, "mime_types") {
                mt_tbl
                    .sequence_values::<String>()
                    .filter_map(|r| r.ok())
                    .collect()
            } else {
                Vec::new()
            };

            let max_file_size = match tbl.get::<mlua::Value>("max_file_size") {
                Ok(mlua::Value::Integer(n)) => Some(n as u64),
                Ok(mlua::Value::String(s)) => {
                    let text = s.to_str().ok().map(|s| s.to_string());
                    text.and_then(|t| crate::config::parse_filesize_string(&t))
                }
                _ => None,
            };

            let image_sizes = if let Ok(sizes_tbl) = get_table(&tbl, "image_sizes") {
                parse_image_sizes(&sizes_tbl)
            } else {
                Vec::new()
            };

            let admin_thumbnail = get_string(&tbl, "admin_thumbnail");
            let format_options = parse_format_options(&tbl);

            let mut upload = CollectionUpload::new();
            upload.mime_types = mime_types;
            upload.max_file_size = max_file_size;
            upload.image_sizes = image_sizes;
            upload.admin_thumbnail = admin_thumbnail;
            upload.format_options = format_options;
            Some(upload)
        }
        _ => None,
    }
}

pub(super) fn parse_image_sizes(tbl: &Table) -> Vec<ImageSize> {
    let mut sizes = Vec::new();
    for size_tbl in tbl.sequence_values::<Table>().flatten() {
        let name = match get_string(&size_tbl, "name") {
            Some(n) => n,
            None => continue,
        };
        let width = size_tbl.get::<u32>("width").unwrap_or(0);
        let height = size_tbl.get::<u32>("height").unwrap_or(0);
        if width == 0 || height == 0 {
            continue;
        }
        let fit = match get_string(&size_tbl, "fit").as_deref() {
            Some("cover") => ImageFit::Cover,
            Some("contain") => ImageFit::Contain,
            Some("inside") => ImageFit::Inside,
            Some("fill") => ImageFit::Fill,
            _ => ImageFit::Cover,
        };
        sizes.push(
            ImageSizeBuilder::new(name)
                .width(width)
                .height(height)
                .fit(fit)
                .build(),
        );
    }
    sizes
}

pub(super) fn parse_format_options(tbl: &Table) -> FormatOptions {
    let fo_tbl = match get_table(tbl, "format_options") {
        Ok(t) => t,
        Err(_) => return FormatOptions::default(),
    };

    let webp = get_table(&fo_tbl, "webp").ok().map(|t| {
        let quality = t.get::<u8>("quality").unwrap_or(80);
        let queue = get_bool(&t, "queue", false);
        FormatQuality::new(quality, queue)
    });

    let avif = get_table(&fo_tbl, "avif").ok().map(|t| {
        let quality = t.get::<u8>("quality").unwrap_or(60);
        let queue = get_bool(&t, "queue", false);
        FormatQuality::new(quality, queue)
    });

    FormatOptions { webp, avif }
}

/// Helper to create a hidden text field definition.
fn hidden_text_field(name: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Text)
        .admin(FieldAdmin::builder().hidden(true).build())
        .build()
}

/// Helper to create a hidden number field definition.
fn hidden_number_field(name: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Number)
        .admin(FieldAdmin::builder().hidden(true).build())
        .build()
}

/// Auto-inject upload metadata fields at position 0 (before user fields).
/// Generates typed columns for each image size instead of a JSON blob.
pub(super) fn inject_upload_fields(fields: &mut Vec<FieldDefinition>, upload: &CollectionUpload) {
    let mut upload_fields = vec![
        FieldDefinition::builder("filename", FieldType::Text)
            .required(true)
            .admin(FieldAdmin::builder().readonly(true).build())
            .build(),
        hidden_text_field("mime_type"),
        hidden_number_field("filesize"),
        hidden_number_field("width"),
        hidden_number_field("height"),
        hidden_text_field("url"),
        hidden_number_field("focal_x"),
        hidden_number_field("focal_y"),
    ];

    // Per-size typed fields: {size}_url, {size}_width, {size}_height
    // Plus format variants: {size}_webp_url, {size}_avif_url
    for size in &upload.image_sizes {
        upload_fields.push(hidden_text_field(&format!("{}_url", size.name)));
        upload_fields.push(hidden_number_field(&format!("{}_width", size.name)));
        upload_fields.push(hidden_number_field(&format!("{}_height", size.name)));

        if upload.format_options.webp.is_some() {
            upload_fields.push(hidden_text_field(&format!("{}_webp_url", size.name)));
        }
        if upload.format_options.avif.is_some() {
            upload_fields.push(hidden_text_field(&format!("{}_avif_url", size.name)));
        }
    }

    // Insert at position 0, before user-defined fields
    for (i, field) in upload_fields.into_iter().enumerate() {
        fields.insert(i, field);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::FieldDefinition;
    use crate::core::upload::{CollectionUpload, FormatOptions, FormatQuality, ImageFit};
    use mlua::Lua;

    #[test]
    fn test_parse_image_sizes_basic() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("name", "thumbnail").unwrap();
        s1.set("width", 200u32).unwrap();
        s1.set("height", 200u32).unwrap();
        tbl.set(1, s1).unwrap();
        let sizes = parse_image_sizes(&tbl);
        assert_eq!(sizes.len(), 1);
        assert_eq!(sizes[0].name, "thumbnail");
        assert_eq!(sizes[0].width, 200);
        assert_eq!(sizes[0].height, 200);
    }

    #[test]
    fn test_parse_image_sizes_with_fit() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        for (i, (name, fit)) in [
            ("a", "cover"),
            ("b", "contain"),
            ("c", "inside"),
            ("d", "fill"),
        ]
        .iter()
        .enumerate()
        {
            let s = lua.create_table().unwrap();
            s.set("name", *name).unwrap();
            s.set("width", 100u32).unwrap();
            s.set("height", 100u32).unwrap();
            s.set("fit", *fit).unwrap();
            tbl.set(i + 1, s).unwrap();
        }
        let sizes = parse_image_sizes(&tbl);
        assert_eq!(sizes.len(), 4);
        assert!(matches!(sizes[0].fit, ImageFit::Cover));
        assert!(matches!(sizes[1].fit, ImageFit::Contain));
        assert!(matches!(sizes[2].fit, ImageFit::Inside));
        assert!(matches!(sizes[3].fit, ImageFit::Fill));
    }

    #[test]
    fn test_parse_image_sizes_skips_missing_name() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("width", 200u32).unwrap();
        s1.set("height", 200u32).unwrap();
        tbl.set(1, s1).unwrap();
        let sizes = parse_image_sizes(&tbl);
        assert!(sizes.is_empty());
    }

    #[test]
    fn test_parse_image_sizes_skips_zero_dimensions() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("name", "bad").unwrap();
        s1.set("width", 0u32).unwrap();
        s1.set("height", 200u32).unwrap();
        tbl.set(1, s1).unwrap();
        let sizes = parse_image_sizes(&tbl);
        assert!(sizes.is_empty());
    }

    #[test]
    fn test_parse_format_options_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let fo = parse_format_options(&tbl);
        assert!(fo.webp.is_none());
        assert!(fo.avif.is_none());
    }

    #[test]
    fn test_parse_format_options_webp_only() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let fo_tbl = lua.create_table().unwrap();
        let webp = lua.create_table().unwrap();
        webp.set("quality", 90u8).unwrap();
        fo_tbl.set("webp", webp).unwrap();
        tbl.set("format_options", fo_tbl).unwrap();
        let fo = parse_format_options(&tbl);
        assert!(fo.webp.is_some());
        assert_eq!(fo.webp.unwrap().quality, 90);
        assert!(fo.avif.is_none());
    }

    #[test]
    fn test_parse_format_options_both() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let fo_tbl = lua.create_table().unwrap();
        let webp = lua.create_table().unwrap();
        webp.set("quality", 75u8).unwrap();
        fo_tbl.set("webp", webp).unwrap();
        let avif = lua.create_table().unwrap();
        avif.set("quality", 50u8).unwrap();
        fo_tbl.set("avif", avif).unwrap();
        tbl.set("format_options", fo_tbl).unwrap();
        let fo = parse_format_options(&tbl);
        assert_eq!(fo.webp.unwrap().quality, 75);
        assert_eq!(fo.avif.unwrap().quality, 50);
    }

    #[test]
    fn test_inject_upload_fields_basic() {
        let mut fields =
            vec![FieldDefinition::builder("alt_text", crate::core::field::FieldType::Text).build()];
        let upload = CollectionUpload::new();
        inject_upload_fields(&mut fields, &upload);
        assert_eq!(fields.len(), 9);
        assert_eq!(fields[0].name, "filename");
        assert_eq!(fields[1].name, "mime_type");
        assert_eq!(fields[2].name, "filesize");
        assert_eq!(fields[3].name, "width");
        assert_eq!(fields[4].name, "height");
        assert_eq!(fields[5].name, "url");
        assert_eq!(fields[6].name, "focal_x");
        assert_eq!(fields[7].name, "focal_y");
        assert_eq!(fields[8].name, "alt_text");
    }

    #[test]
    fn test_inject_upload_fields_with_image_sizes() {
        let mut fields = Vec::new();
        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("thumb")
                .width(200)
                .height(200)
                .fit(ImageFit::Cover)
                .build(),
        ];
        inject_upload_fields(&mut fields, &upload);
        assert_eq!(fields.len(), 11);
        assert_eq!(fields[8].name, "thumb_url");
        assert_eq!(fields[9].name, "thumb_width");
        assert_eq!(fields[10].name, "thumb_height");
    }

    #[test]
    fn test_inject_upload_fields_with_format_variants() {
        let mut fields = Vec::new();
        let mut upload = CollectionUpload::new();
        upload.image_sizes = vec![
            ImageSizeBuilder::new("card")
                .width(400)
                .height(300)
                .fit(ImageFit::Cover)
                .build(),
        ];
        upload.format_options = FormatOptions {
            webp: Some(FormatQuality::new(80, false)),
            avif: Some(FormatQuality::new(60, false)),
        };
        inject_upload_fields(&mut fields, &upload);
        assert_eq!(fields.len(), 13);
        let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"card_webp_url"));
        assert!(names.contains(&"card_avif_url"));
    }

    #[test]
    fn test_parse_collection_upload_true() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("upload", true).unwrap();
        let upload = parse_collection_upload(&tbl);
        assert!(upload.is_some());
        assert!(upload.unwrap().enabled);
    }

    #[test]
    fn test_parse_collection_upload_false() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("upload", false).unwrap();
        assert!(parse_collection_upload(&tbl).is_none());
    }

    #[test]
    fn test_parse_collection_upload_table_with_details() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        let mime_types = lua.create_table().unwrap();
        mime_types.set(1, "image/png").unwrap();
        mime_types.set(2, "image/jpeg").unwrap();
        upload_tbl.set("mime_types", mime_types).unwrap();
        upload_tbl.set("max_file_size", 5000000u64).unwrap();
        upload_tbl.set("admin_thumbnail", "thumb").unwrap();

        let sizes = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("name", "thumb").unwrap();
        s1.set("width", 200u32).unwrap();
        s1.set("height", 200u32).unwrap();
        sizes.set(1, s1).unwrap();
        upload_tbl.set("image_sizes", sizes).unwrap();

        tbl.set("upload", upload_tbl).unwrap();
        let upload = parse_collection_upload(&tbl).unwrap();
        assert!(upload.enabled);
        assert_eq!(upload.mime_types, vec!["image/png", "image/jpeg"]);
        assert_eq!(upload.max_file_size, Some(5000000));
        assert_eq!(upload.admin_thumbnail.as_deref(), Some("thumb"));
        assert_eq!(upload.image_sizes.len(), 1);
        assert_eq!(upload.image_sizes[0].name, "thumb");
    }

    #[test]
    fn test_parse_collection_upload_max_file_size_integer() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        upload_tbl.set("max_file_size", 1048576i64).unwrap();
        tbl.set("upload", upload_tbl).unwrap();
        let upload = parse_collection_upload(&tbl).unwrap();
        assert_eq!(upload.max_file_size, Some(1048576));
    }

    #[test]
    fn test_parse_collection_upload_max_file_size_string() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        upload_tbl.set("max_file_size", "10MB").unwrap();
        tbl.set("upload", upload_tbl).unwrap();
        let upload = parse_collection_upload(&tbl).unwrap();
        assert_eq!(upload.max_file_size, Some(10 * 1024 * 1024));
    }

    #[test]
    fn test_parse_collection_upload_other_value_returns_none() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let func = lua.create_function(|_, ()| Ok(())).unwrap();
        tbl.set("upload", func).unwrap();
        assert!(parse_collection_upload(&tbl).is_none());
    }

    #[test]
    fn test_parse_format_options_queue_flag() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let fo_tbl = lua.create_table().unwrap();
        let webp = lua.create_table().unwrap();
        webp.set("quality", 85u8).unwrap();
        webp.set("queue", true).unwrap();
        fo_tbl.set("webp", webp).unwrap();
        let avif = lua.create_table().unwrap();
        avif.set("quality", 65u8).unwrap();
        avif.set("queue", true).unwrap();
        fo_tbl.set("avif", avif).unwrap();
        tbl.set("format_options", fo_tbl).unwrap();
        let fo = parse_format_options(&tbl);
        assert!(fo.webp.as_ref().unwrap().queue);
        assert_eq!(fo.webp.as_ref().unwrap().quality, 85);
        assert!(fo.avif.as_ref().unwrap().queue);
        assert_eq!(fo.avif.as_ref().unwrap().quality, 65);
    }

    #[test]
    fn test_parse_format_options_avif_only() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let fo_tbl = lua.create_table().unwrap();
        let avif = lua.create_table().unwrap();
        avif.set("quality", 55u8).unwrap();
        fo_tbl.set("avif", avif).unwrap();
        tbl.set("format_options", fo_tbl).unwrap();
        let fo = parse_format_options(&tbl);
        assert!(fo.webp.is_none());
        assert_eq!(fo.avif.as_ref().unwrap().quality, 55);
    }

    #[test]
    fn test_parse_image_sizes_skips_zero_height() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("name", "bad_h").unwrap();
        s1.set("width", 200u32).unwrap();
        s1.set("height", 0u32).unwrap();
        tbl.set(1, s1).unwrap();
        let sizes = parse_image_sizes(&tbl);
        assert!(sizes.is_empty());
    }

    #[test]
    fn test_parse_image_sizes_unknown_fit_defaults_to_cover() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s = lua.create_table().unwrap();
        s.set("name", "banner").unwrap();
        s.set("width", 1200u32).unwrap();
        s.set("height", 400u32).unwrap();
        s.set("fit", "stretch").unwrap();
        tbl.set(1, s).unwrap();
        let sizes = parse_image_sizes(&tbl);
        assert_eq!(sizes.len(), 1);
        assert!(matches!(sizes[0].fit, ImageFit::Cover));
    }

    #[test]
    fn test_parse_collection_upload_table_no_mime_types() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        tbl.set("upload", upload_tbl).unwrap();
        let upload = parse_collection_upload(&tbl).unwrap();
        assert!(upload.mime_types.is_empty());
        assert!(upload.max_file_size.is_none());
    }
}
