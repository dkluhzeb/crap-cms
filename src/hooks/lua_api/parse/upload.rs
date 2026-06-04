//! Parsing functions for collection upload configuration.

use mlua::{Error::RuntimeError, Result as LuaResult, Table, Value};

use crate::{
    config::parse_filesize_string,
    core::{
        FieldAdmin, FieldDefinition, FieldType,
        upload::{
            CollectionUpload, FormatOptions, FormatQuality, ImageFit, ImageSize, ImageSizeBuilder,
        },
    },
};

use super::helpers::{get_bool, get_string, get_table};

pub(super) fn parse_collection_upload(config: &Table) -> LuaResult<Option<CollectionUpload>> {
    let val: Value = match config.get("upload") {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    match val {
        Value::Boolean(true) => Ok(Some(CollectionUpload::new())),
        Value::Table(tbl) => {
            // `enabled = false` disables upload, mirroring `upload = false`.
            if !get_bool(&tbl, "enabled", true)? {
                return Ok(None);
            }

            let mime_types = if let Ok(mt_tbl) = get_table(&tbl, "mime_types") {
                mt_tbl
                    .sequence_values::<String>()
                    .filter_map(std::result::Result::ok)
                    .collect()
            } else {
                Vec::new()
            };

            let max_file_size = parse_max_file_size(&tbl)?;

            let image_sizes = if let Ok(sizes_tbl) = get_table(&tbl, "image_sizes") {
                parse_image_sizes(&sizes_tbl)?
            } else {
                Vec::new()
            };

            let admin_thumbnail = get_string(&tbl, "admin_thumbnail");
            let format_options = parse_format_options(&tbl)?;

            let mut upload = CollectionUpload::new();

            upload.mime_types = mime_types;
            upload.max_file_size = max_file_size;
            upload.image_sizes = image_sizes;
            upload.admin_thumbnail = admin_thumbnail;
            upload.format_options = format_options;

            Ok(Some(upload))
        }
        _ => Ok(None),
    }
}

/// Parse `upload.max_file_size`. Absent (`nil`) means "inherit the global
/// `[upload] max_file_size`". A present-but-malformed value (negative integer,
/// unparseable string, wrong type) is a hard error rather than a silent
/// fall-back to the global default — parity with the strict schema-key checks.
fn parse_max_file_size(tbl: &Table) -> LuaResult<Option<u64>> {
    match tbl.get::<Value>("max_file_size")? {
        Value::Nil => Ok(None),
        Value::Integer(n) => u64::try_from(n).map(Some).map_err(|_| {
            RuntimeError(format!(
                "upload max_file_size must be a non-negative byte count, got {n}"
            ))
        }),
        Value::String(s) => {
            let text = s.to_str().map(|t| t.to_string()).map_err(|_| {
                RuntimeError("upload max_file_size string is not valid UTF-8".to_string())
            })?;

            parse_filesize_string(&text).map(Some).ok_or_else(|| {
                RuntimeError(format!(
                    "upload max_file_size '{text}' is not a valid size \
                     (use a byte count or a string like \"10MB\", \"1GB\")"
                ))
            })
        }
        other => Err(RuntimeError(format!(
            "upload max_file_size must be an integer or string, got {}",
            other.type_name()
        ))),
    }
}

/// Read a required positive pixel dimension (`width`/`height`) from an image
/// size entry. Missing, non-integer, or non-positive values are hard errors
/// instead of silently dropping the whole size entry.
fn require_dimension(tbl: &Table, key: &str, size_name: &str) -> LuaResult<u32> {
    let value = tbl.get::<Option<u32>>(key).map_err(|_| {
        RuntimeError(format!(
            "image size '{size_name}' has an invalid '{key}' (expected a positive integer)"
        ))
    })?;

    match value {
        Some(v) if v > 0 => Ok(v),
        _ => Err(RuntimeError(format!(
            "image size '{size_name}' is missing a positive '{key}'"
        ))),
    }
}

/// Parse the optional `fit` mode of an image size entry. An unknown value is a
/// hard error (it used to silently fall back to `cover`, hiding typos).
fn parse_fit(tbl: &Table, size_name: &str) -> LuaResult<ImageFit> {
    match get_string(tbl, "fit").as_deref() {
        None | Some("cover") => Ok(ImageFit::Cover),
        Some("contain") => Ok(ImageFit::Contain),
        Some("inside") => Ok(ImageFit::Inside),
        Some("fill") => Ok(ImageFit::Fill),
        Some(other) => Err(RuntimeError(format!(
            "image size '{size_name}' has an unknown fit '{other}'. \
             Valid values: cover, contain, inside, fill"
        ))),
    }
}

pub(super) fn parse_image_sizes(tbl: &Table) -> LuaResult<Vec<ImageSize>> {
    let mut sizes = Vec::new();

    for (idx, item) in tbl.sequence_values::<Table>().enumerate() {
        let pos = idx + 1;

        let size_tbl = item.map_err(|_| {
            RuntimeError(format!(
                "image_sizes[{pos}] must be a table with name/width/height"
            ))
        })?;

        let name = match get_string(&size_tbl, "name") {
            Some(name) if !name.is_empty() => name,
            _ => {
                return Err(RuntimeError(format!(
                    "image_sizes[{pos}] is missing a non-empty 'name'"
                )));
            }
        };

        let width = require_dimension(&size_tbl, "width", &name)?;
        let height = require_dimension(&size_tbl, "height", &name)?;
        let fit = parse_fit(&size_tbl, &name)?;

        sizes.push(
            ImageSizeBuilder::new(name)
                .width(width)
                .height(height)
                .fit(fit)
                .build(),
        );
    }

    Ok(sizes)
}

pub(super) fn parse_format_options(tbl: &Table) -> LuaResult<FormatOptions> {
    let Ok(fo_tbl) = get_table(tbl, "format_options") else {
        return Ok(FormatOptions::default());
    };

    let webp = match get_table(&fo_tbl, "webp") {
        Ok(t) => {
            let quality = t.get::<u8>("quality").unwrap_or(80);
            let queue = get_bool(&t, "queue", false)?;
            Some(FormatQuality::new(quality, queue))
        }
        Err(_) => None,
    };

    let avif = match get_table(&fo_tbl, "avif") {
        Ok(t) => {
            let quality = t.get::<u8>("quality").unwrap_or(60);
            let queue = get_bool(&t, "queue", false)?;
            Some(FormatQuality::new(quality, queue))
        }
        Err(_) => None,
    };

    Ok(FormatOptions { webp, avif })
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
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use super::*;
    use crate::core::{
        FieldDefinition, FieldType,
        upload::{CollectionUpload, FormatOptions, FormatQuality, ImageFit},
    };
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
        let sizes = parse_image_sizes(&tbl).unwrap();
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
        let sizes = parse_image_sizes(&tbl).unwrap();
        assert_eq!(sizes.len(), 4);
        assert!(matches!(sizes[0].fit, ImageFit::Cover));
        assert!(matches!(sizes[1].fit, ImageFit::Contain));
        assert!(matches!(sizes[2].fit, ImageFit::Inside));
        assert!(matches!(sizes[3].fit, ImageFit::Fill));
    }

    /// Regression: an image size entry missing `name` is a hard error, not a
    /// silently dropped entry. `name` is documented as required.
    #[test]
    fn test_parse_image_sizes_rejects_missing_name() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("width", 200u32).unwrap();
        s1.set("height", 200u32).unwrap();
        tbl.set(1, s1).unwrap();
        let err = parse_image_sizes(&tbl).unwrap_err();
        assert!(err.to_string().contains("name"), "{err}");
    }

    /// Regression: a zero/missing `width` is a hard error, not a silently
    /// dropped entry.
    #[test]
    fn test_parse_image_sizes_rejects_zero_width() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("name", "bad").unwrap();
        s1.set("width", 0u32).unwrap();
        s1.set("height", 200u32).unwrap();
        tbl.set(1, s1).unwrap();
        let err = parse_image_sizes(&tbl).unwrap_err();
        assert!(err.to_string().contains("width"), "{err}");
    }

    /// Regression: a missing `width` key (not merely zero) is also rejected.
    #[test]
    fn test_parse_image_sizes_rejects_missing_width() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("name", "bad").unwrap();
        s1.set("height", 200u32).unwrap();
        tbl.set(1, s1).unwrap();
        let err = parse_image_sizes(&tbl).unwrap_err();
        assert!(err.to_string().contains("width"), "{err}");
    }

    #[test]
    fn test_parse_format_options_absent() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let fo = parse_format_options(&tbl).unwrap();
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
        let fo = parse_format_options(&tbl).unwrap();
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
        let fo = parse_format_options(&tbl).unwrap();
        assert_eq!(fo.webp.unwrap().quality, 75);
        assert_eq!(fo.avif.unwrap().quality, 50);
    }

    #[test]
    fn test_inject_upload_fields_basic() {
        let mut fields = vec![FieldDefinition::builder("alt_text", FieldType::Text).build()];
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
        let upload = parse_collection_upload(&tbl).unwrap();
        assert!(upload.is_some());
        assert!(upload.unwrap().enabled);
    }

    #[test]
    fn test_parse_collection_upload_false() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        tbl.set("upload", false).unwrap();
        assert!(parse_collection_upload(&tbl).unwrap().is_none());
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
        let upload = parse_collection_upload(&tbl).unwrap().unwrap();
        assert!(upload.enabled);
        assert_eq!(upload.mime_types, vec!["image/png", "image/jpeg"]);
        assert_eq!(upload.max_file_size, Some(5000000));
        assert_eq!(upload.admin_thumbnail.as_deref(), Some("thumb"));
        assert_eq!(upload.image_sizes.len(), 1);
        assert_eq!(upload.image_sizes[0].name, "thumb");
    }

    #[test]
    fn test_parse_collection_upload_enabled_false_disables() {
        // Regression: `upload = { enabled = false }` must disable upload,
        // matching `upload = false`. Previously `enabled` was silently ignored.
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        upload_tbl.set("enabled", false).unwrap();
        tbl.set("upload", upload_tbl).unwrap();
        assert!(parse_collection_upload(&tbl).unwrap().is_none());
    }

    #[test]
    fn test_parse_collection_upload_enabled_true_is_accepted() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        upload_tbl.set("enabled", true).unwrap();
        tbl.set("upload", upload_tbl).unwrap();
        assert!(parse_collection_upload(&tbl).unwrap().unwrap().enabled);
    }

    #[test]
    fn test_parse_collection_upload_max_file_size_integer() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        upload_tbl.set("max_file_size", 1048576i64).unwrap();
        tbl.set("upload", upload_tbl).unwrap();
        let upload = parse_collection_upload(&tbl).unwrap().unwrap();
        assert_eq!(upload.max_file_size, Some(1048576));
    }

    #[test]
    fn test_parse_collection_upload_max_file_size_string() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        upload_tbl.set("max_file_size", "10MB").unwrap();
        tbl.set("upload", upload_tbl).unwrap();
        let upload = parse_collection_upload(&tbl).unwrap().unwrap();
        assert_eq!(upload.max_file_size, Some(10 * 1024 * 1024));
    }

    /// Absent `max_file_size` inherits the global `[upload]` default (None here).
    #[test]
    fn test_parse_collection_upload_max_file_size_absent_is_none() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        tbl.set("upload", upload_tbl).unwrap();
        let upload = parse_collection_upload(&tbl).unwrap().unwrap();
        assert_eq!(upload.max_file_size, None);
    }

    /// Regression: a malformed `max_file_size` string is a hard error, not a
    /// silent fall-back to the global default.
    #[test]
    fn test_parse_collection_upload_max_file_size_bad_string_errors() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        upload_tbl.set("max_file_size", "10MBB").unwrap();
        tbl.set("upload", upload_tbl).unwrap();
        let err = parse_collection_upload(&tbl).unwrap_err();
        assert!(err.to_string().contains("max_file_size"), "{err}");
    }

    /// Regression: a negative `max_file_size` is a hard error rather than
    /// silently using the global default.
    #[test]
    fn test_parse_collection_upload_max_file_size_negative_errors() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        upload_tbl.set("max_file_size", -1i64).unwrap();
        tbl.set("upload", upload_tbl).unwrap();
        let err = parse_collection_upload(&tbl).unwrap_err();
        assert!(err.to_string().contains("max_file_size"), "{err}");
    }

    /// Regression: a wrong-typed `max_file_size` (e.g. boolean) is a hard error.
    #[test]
    fn test_parse_collection_upload_max_file_size_wrong_type_errors() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        upload_tbl.set("max_file_size", true).unwrap();
        tbl.set("upload", upload_tbl).unwrap();
        let err = parse_collection_upload(&tbl).unwrap_err();
        assert!(err.to_string().contains("max_file_size"), "{err}");
    }

    #[test]
    fn test_parse_collection_upload_other_value_returns_none() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let func = lua.create_function(|_, ()| Ok(())).unwrap();
        tbl.set("upload", func).unwrap();
        assert!(parse_collection_upload(&tbl).unwrap().is_none());
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
        let fo = parse_format_options(&tbl).unwrap();
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
        let fo = parse_format_options(&tbl).unwrap();
        assert!(fo.webp.is_none());
        assert_eq!(fo.avif.as_ref().unwrap().quality, 55);
    }

    /// Regression: a zero `height` is a hard error, not a silently dropped
    /// entry.
    #[test]
    fn test_parse_image_sizes_rejects_zero_height() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s1 = lua.create_table().unwrap();
        s1.set("name", "bad_h").unwrap();
        s1.set("width", 200u32).unwrap();
        s1.set("height", 0u32).unwrap();
        tbl.set(1, s1).unwrap();
        let err = parse_image_sizes(&tbl).unwrap_err();
        assert!(err.to_string().contains("height"), "{err}");
    }

    /// Regression: an unknown `fit` value is a hard error. It used to silently
    /// fall back to `cover`, hiding typos like `"covr"`.
    #[test]
    fn test_parse_image_sizes_rejects_unknown_fit() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let s = lua.create_table().unwrap();
        s.set("name", "banner").unwrap();
        s.set("width", 1200u32).unwrap();
        s.set("height", 400u32).unwrap();
        s.set("fit", "stretch").unwrap();
        tbl.set(1, s).unwrap();
        let err = parse_image_sizes(&tbl).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("stretch"), "should name the bad value: {msg}");
        assert!(msg.contains("fit"), "should mention fit: {msg}");
    }

    #[test]
    fn test_parse_collection_upload_table_no_mime_types() {
        let lua = Lua::new();
        let tbl = lua.create_table().unwrap();
        let upload_tbl = lua.create_table().unwrap();
        tbl.set("upload", upload_tbl).unwrap();
        let upload = parse_collection_upload(&tbl).unwrap().unwrap();
        assert!(upload.mime_types.is_empty());
        assert!(upload.max_file_size.is_none());
    }
}
