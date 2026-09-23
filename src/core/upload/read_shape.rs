//! The field shapes a client reads and writes.
//!
//! A document's stored fields are not the shape either direction of the wire
//! carries:
//!
//! - **Read.** [`shape_read_document`] folds an upload collection's per-size
//!   columns into one nested `sizes` object, and every read strips the fields
//!   marked `hidden = true` (at any depth). [`read_shape_fields`] makes the
//!   same statement about the *schema*.
//! - **Write.** A write can never store a virtual `Join` field
//!   ([`FieldType::is_writable`]), and the server-derived upload columns
//!   ([`CollectionUpload::derived_field_names`]) are stripped from untrusted
//!   input. [`write_shape_fields`] is the schema a caller may send.
//!
//! Every outward-facing description of the wire — the client SDK type
//! generators, the Lua type generator, the proto decoder — derives from here
//! instead of walking the raw field list, so a description can never declare a
//! key the wire does not carry or accept.
//!
//! [`shape_read_document`]: crate::core::upload::shape_read_document

use std::borrow::Cow;

use crate::core::{CollectionDefinition, FieldDefinition, FieldType, upload::CollectionUpload};

/// The wire key the per-size columns collapse into on every read.
pub const SIZES_FIELD: &str = "sizes";

/// Which fields a filtered field list keeps.
type Keep = dyn Fn(&FieldDefinition) -> bool;

/// The fields a read of `def` returns.
///
/// For an upload collection with image sizes that is the collection's fields
/// with the per-size columns (`{size}_url`, `{size}_width`, `{size}_height`,
/// and each format variant's `{size}_{format}_url`) replaced by the nested
/// [`SIZES_FIELD`] object. `hidden` fields are dropped at any depth (see
/// [`readable_fields`]).
#[must_use]
pub fn read_shape_fields(def: &CollectionDefinition) -> Cow<'_, [FieldDefinition]> {
    let Some(upload) = def
        .upload
        .as_ref()
        .filter(|u| u.enabled && !u.image_sizes.is_empty())
    else {
        return readable_fields(&def.fields);
    };

    let folded = fold_size_columns(&def.fields, upload);

    Cow::Owned(readable_fields(&folded).into_owned())
}

/// `fields` without the ones every read strips: a field marked `hidden = true`
/// is removed from each read response, inside groups, layout wrappers, array
/// rows and blocks included. The read shape of a global's fields.
#[must_use]
pub fn readable_fields(fields: &[FieldDefinition]) -> Cow<'_, [FieldDefinition]> {
    retain_deep(fields, &|f: &FieldDefinition| !f.hidden)
}

/// The fields a create or update of `def` accepts as data.
///
/// That is [`writable_fields`] minus, on an upload collection, the
/// server-derived upload columns ([`CollectionUpload::derived_field_names`]) —
/// the exact set the write chokepoint strips from untrusted input, so the two
/// can never disagree. An auth collection's `password` is not a field and is
/// not part of this list.
#[must_use]
pub fn write_shape_fields(def: &CollectionDefinition) -> Cow<'_, [FieldDefinition]> {
    let writable = writable_fields(&def.fields);

    let Some(upload) = def.upload.as_ref() else {
        return writable;
    };

    let derived = upload.derived_field_names();

    Cow::Owned(
        writable
            .iter()
            .filter(|f| !derived.contains(&f.name))
            .cloned()
            .collect(),
    )
}

/// `fields` without the ones a write can never store: the virtual `Join`
/// fields ([`FieldType::is_writable`]), at any depth. The write shape of a
/// global's fields.
#[must_use]
pub fn writable_fields(fields: &[FieldDefinition]) -> Cow<'_, [FieldDefinition]> {
    retain_deep(fields, &|f: &FieldDefinition| f.field_type.is_writable())
}

/// `fields` keeping only what `keep` accepts, at every depth. Borrowed when
/// nothing is dropped.
fn retain_deep<'a>(fields: &'a [FieldDefinition], keep: &Keep) -> Cow<'a, [FieldDefinition]> {
    if !drops_any(fields, keep) {
        return Cow::Borrowed(fields);
    }

    Cow::Owned(
        fields
            .iter()
            .filter(|&f| keep(f))
            .map(|f| retain_children(f, keep))
            .collect(),
    )
}

/// Whether `keep` rejects any field in `fields` or beneath them.
fn drops_any(fields: &[FieldDefinition], keep: &Keep) -> bool {
    fields.iter().any(|f| {
        !keep(f)
            || drops_any(&f.fields, keep)
            || f.tabs.iter().any(|tab| drops_any(&tab.fields, keep))
            || f.blocks.iter().any(|block| drops_any(&block.fields, keep))
    })
}

/// `field` with its sub-fields, tab fields and block fields filtered by `keep`.
fn retain_children(field: &FieldDefinition, keep: &Keep) -> FieldDefinition {
    let mut out = field.clone();

    out.fields = retain_deep(&field.fields, keep).into_owned();

    for tab in &mut out.tabs {
        tab.fields = retain_deep(&tab.fields, keep).into_owned();
    }

    for block in &mut out.blocks {
        block.fields = retain_deep(&block.fields, keep).into_owned();
    }

    out
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
    use crate::core::{
        BlockDefinition, FieldTab,
        collection::Auth,
        upload::{FormatQuality, ImageSizeBuilder},
    };

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

    fn text(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text).build()
    }

    fn hidden(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .hidden(true)
            .build()
    }

    fn join(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Join).build()
    }

    /// Regression: a `hidden = true` field is stripped from every read, but the
    /// read shape declared it — at the top level and nested alike.
    #[test]
    fn hidden_fields_are_not_part_of_the_read_shape() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            text("title"),
            hidden("secret"),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![text("meta"), hidden("internal")])
                .build(),
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![hidden("in_row")])
                .build(),
            FieldDefinition::builder("tabs", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "T",
                    vec![hidden("in_tab"), text("kept")],
                )])
                .build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![text("label"), hidden("row_secret")])
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "hero",
                    vec![text("heading"), hidden("block_secret")],
                )])
                .build(),
        ];

        let shaped = read_shape_fields(&def);

        assert_eq!(
            names(&shaped),
            ["title", "seo", "row", "tabs", "items", "content"]
        );
        assert_eq!(names(&shaped[1].fields), ["meta"]);
        assert!(shaped[2].fields.is_empty());
        assert_eq!(names(&shaped[3].tabs[0].fields), ["kept"]);
        assert_eq!(names(&shaped[4].fields), ["label"]);
        assert_eq!(names(&shaped[5].blocks[0].fields), ["heading"]);
    }

    /// A global has no upload shape; its read shape only drops hidden fields.
    #[test]
    fn readable_fields_borrow_when_nothing_is_hidden() {
        let fields = vec![text("title")];

        assert!(matches!(readable_fields(&fields), Cow::Borrowed(_)));
    }

    /// An upload collection's read shape drops hidden fields as well as
    /// folding the per-size columns.
    #[test]
    fn upload_read_shape_also_drops_hidden_fields() {
        let mut def = upload_collection(&["thumbnail"], false, false);
        def.fields.push(hidden("secret"));

        let shaped = read_shape_fields(&def);

        assert_eq!(names(&shaped), [SIZES_FIELD, "alt"]);
    }

    /// Regression: the write shape declared the server-derived upload columns
    /// (the write chokepoint strips them) and the virtual `Join` fields (a
    /// write can never store them). The focal point and hidden fields stay
    /// writable.
    #[test]
    fn write_shape_drops_derived_upload_columns_and_joins() {
        let mut def = upload_collection(&["thumbnail"], true, false);
        let mut fields: Vec<FieldDefinition> =
            ["filename", "mime_type", "url", "focal_x", "focal_y"]
                .map(text)
                .into();
        fields.append(&mut def.fields);
        def.fields = fields;
        def.fields.push(join("mentions"));
        def.fields.push(hidden("secret"));

        let shaped = write_shape_fields(&def);

        assert_eq!(names(&shaped), ["focal_x", "focal_y", "alt", "secret"]);
    }

    /// The upload columns stripped from the write shape are exactly the ones
    /// the write chokepoint strips from untrusted input.
    #[test]
    fn write_shape_strips_exactly_the_derived_names() {
        let mut def = upload_collection(&["card"], false, true);
        let derived = def.upload.as_ref().expect("upload").derived_field_names();
        def.fields.extend(derived.iter().map(|name| text(name)));

        let shaped = write_shape_fields(&def);

        assert!(shaped.iter().all(|f| !derived.contains(&f.name)));
        assert_eq!(names(&shaped), ["alt"]);
    }

    #[test]
    fn writable_fields_drop_nested_joins_and_borrow_otherwise() {
        let fields = vec![
            text("title"),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![text("meta"), join("refs")])
                .build(),
        ];

        let shaped = writable_fields(&fields);
        assert_eq!(names(&shaped[1].fields), ["meta"]);

        let plain = vec![text("title")];
        assert!(matches!(writable_fields(&plain), Cow::Borrowed(_)));
    }

    /// An auth collection's `password` is not a field: the write shape is the
    /// fields alone.
    #[test]
    fn auth_write_shape_is_the_fields_alone() {
        let mut def = CollectionDefinition::new("users");
        def.auth = Some(Auth::new(true));
        def.fields = vec![text("email")];

        assert_eq!(names(&write_shape_fields(&def)), ["email"]);
    }
}
