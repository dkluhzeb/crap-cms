//! The field shape a read of an upload collection returns.
//!
//! [`shape_read_document`] folds an upload collection's per-size columns into
//! one nested `sizes` object on every read. This module makes the same
//! statement about the *schema*: which fields a reader actually receives. Every
//! outward-facing description of the read wire — the client SDK type
//! generators, the proto decoder — derives from here instead of walking the raw
//! field list, so a description can never again declare columns the wire does
//! not carry.
//!
//! The WRITE shape is untouched: the per-size columns are server-derived and
//! already stripped from untrusted input
//! ([`CollectionUpload::derived_field_names`]).
//!
//! [`shape_read_document`]: crate::core::upload::shape_read_document

use std::borrow::Cow;

use crate::core::{CollectionDefinition, FieldDefinition, FieldType, upload::CollectionUpload};

/// The wire key the per-size columns collapse into on every read.
pub const SIZES_FIELD: &str = "sizes";

/// The fields a read of `def` returns.
///
/// For an upload collection with image sizes that is the collection's fields
/// with the per-size columns (`{size}_url`, `{size}_width`, `{size}_height`,
/// and each format variant's `{size}_{format}_url`) replaced by the nested
/// [`SIZES_FIELD`] object. Every other collection's fields are returned as they
/// are stored.
#[must_use]
pub fn read_shape_fields(def: &CollectionDefinition) -> Cow<'_, [FieldDefinition]> {
    let Some(upload) = def.upload.as_ref().filter(|u| u.enabled) else {
        return Cow::Borrowed(&def.fields);
    };

    if upload.image_sizes.is_empty() {
        return Cow::Borrowed(&def.fields);
    }

    Cow::Owned(fold_size_columns(&def.fields, upload))
}

/// `fields` with the per-size columns dropped and the `sizes` object inserted
/// where the first of them stood.
fn fold_size_columns(
    fields: &[FieldDefinition],
    upload: &CollectionUpload,
) -> Vec<FieldDefinition> {
    let stripped: Vec<String> = upload
        .size_columns()
        .into_iter()
        .map(|(name, _)| name)
        .collect();

    let at = fields
        .iter()
        .position(|f| stripped.contains(&f.name))
        .unwrap_or(fields.len());

    let mut out: Vec<FieldDefinition> = fields
        .iter()
        .filter(|f| !stripped.contains(&f.name))
        .cloned()
        .collect();

    out.insert(at.min(out.len()), sizes_field(upload));

    out
}

/// The `sizes` object: one entry per configured image size, keyed by size name.
fn sizes_field(upload: &CollectionUpload) -> FieldDefinition {
    let entries = upload
        .image_sizes
        .iter()
        .map(|size| size_entry_field(&size.name, upload))
        .collect();

    group(SIZES_FIELD, entries)
}

/// One `sizes.{name}` entry: the resized file's `url`, its pixel dimensions,
/// and — when format variants are configured — a `formats` object of `{ url }`
/// entries keyed by format name.
///
/// The entry itself is optional (a size whose resize produced no file is left
/// out entirely), while the `url` inside a present entry always exists; the
/// dimensions are written only when the resize reported them.
fn size_entry_field(name: &str, upload: &CollectionUpload) -> FieldDefinition {
    let mut fields = vec![url_field(), number("width"), number("height")];

    let formats: Vec<FieldDefinition> = upload
        .format_variants()
        .into_iter()
        .map(|format| group(format, vec![url_field()]))
        .collect();

    if !formats.is_empty() {
        fields.push(group("formats", formats));
    }

    group(name, fields)
}

/// A nested object of the `sizes` shape.
fn group(name: &str, fields: Vec<FieldDefinition>) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Group)
        .fields(fields)
        .build()
}

/// The served URL a present entry always carries.
fn url_field() -> FieldDefinition {
    FieldDefinition::builder("url", FieldType::Text)
        .required(true)
        .build()
}

/// A pixel dimension, left out when the stored column is blank.
fn number(name: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Number).build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::upload::{FormatQuality, ImageSizeBuilder};

    fn upload_collection(sizes: &[&str], webp: bool, avif: bool) -> CollectionDefinition {
        let mut upload = CollectionUpload::new();
        upload.image_sizes = sizes
            .iter()
            .map(|name| ImageSizeBuilder::new(*name).width(200).height(200).build())
            .collect();
        if webp {
            upload.format_options.webp = Some(FormatQuality::new(80, false));
        }
        if avif {
            upload.format_options.avif = Some(FormatQuality::new(60, false));
        }

        let mut def = CollectionDefinition::new("media");
        def.fields = upload
            .size_columns()
            .into_iter()
            .map(|(name, ty)| FieldDefinition::builder(name, ty).build())
            .collect();
        def.fields
            .push(FieldDefinition::builder("alt", FieldType::Text).build());
        def.upload = Some(upload);

        def
    }

    fn names(fields: &[FieldDefinition]) -> Vec<&str> {
        fields.iter().map(|f| f.name.as_str()).collect()
    }

    #[test]
    fn a_collection_without_upload_keeps_its_fields() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];

        let shaped = read_shape_fields(&def);

        assert!(matches!(shaped, Cow::Borrowed(_)));
        assert_eq!(names(&shaped), ["title"]);
    }

    /// An upload collection with no image sizes has no per-size columns and so
    /// no `sizes` object either.
    #[test]
    fn an_upload_without_image_sizes_keeps_its_fields() {
        let mut def = CollectionDefinition::new("media");
        def.fields = vec![FieldDefinition::builder("filename", FieldType::Text).build()];
        def.upload = Some(CollectionUpload::new());

        let shaped = read_shape_fields(&def);

        assert!(matches!(shaped, Cow::Borrowed(_)));
        assert_eq!(names(&shaped), ["filename"]);
    }

    /// The per-size columns are exactly the ones a read strips, and the `sizes`
    /// object takes their place — the shape the wire actually carries.
    #[test]
    fn per_size_columns_are_replaced_by_the_sizes_object() {
        let def = upload_collection(&["thumbnail"], true, false);

        let shaped = read_shape_fields(&def);

        assert_eq!(names(&shaped), [SIZES_FIELD, "alt"]);
    }

    /// The entry shape mirrors what the read assembles: a required `url`,
    /// optional dimensions, and a `formats` object per configured variant.
    #[test]
    fn a_size_entry_carries_url_dimensions_and_format_variants() {
        let def = upload_collection(&["thumbnail"], true, true);
        let shaped = read_shape_fields(&def);

        let sizes = &shaped[0];
        assert_eq!(sizes.field_type, FieldType::Group);
        assert_eq!(names(&sizes.fields), ["thumbnail"]);

        let entry = &sizes.fields[0];
        assert_eq!(names(&entry.fields), ["url", "width", "height", "formats"]);
        assert!(entry.fields[0].required, "a present entry always has a url");
        assert!(!entry.fields[1].required);

        let formats = &entry.fields[3];
        assert_eq!(names(&formats.fields), ["webp", "avif"]);
        assert_eq!(names(&formats.fields[0].fields), ["url"]);
    }

    /// Without format options there is no `formats` object to describe.
    #[test]
    fn no_format_options_means_no_formats_object() {
        let def = upload_collection(&["card"], false, false);
        let shaped = read_shape_fields(&def);

        let entry = &shaped[0].fields[0];
        assert_eq!(names(&entry.fields), ["url", "width", "height"]);
    }

    #[test]
    fn every_configured_size_gets_an_entry() {
        let def = upload_collection(&["thumbnail", "card"], false, false);
        let shaped = read_shape_fields(&def);

        assert_eq!(names(&shaped[0].fields), ["thumbnail", "card"]);
    }
}
