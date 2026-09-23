//! Single-field rendering: `write_field` (emits a single
//! `---@field name? type` line) and `field_to_lua_type` (maps a
//! [`FieldDefinition`] to its `LuaLS` type string) — each in a [`LuaShape`],
//! the direction of the wire a class describes.

use std::slice::from_ref;

use crate::{
    core::{FieldDefinition, FieldType, flatten_array_sub_fields},
    typegen::{
        helpers::{is_optional, rel_has_many, to_pascal_case, w},
        idents::{escape_str, lua_field_key},
    },
};

/// Which wire shape a Lua class describes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LuaShape {
    /// What a caller writes and a hook's `ctx.data` holds: a reference is its
    /// id (a polymorphic one its `"collection/id"` string), a JSON rich text
    /// value the document or its JSON text. Each field keeps its own
    /// optionality.
    Input,
    /// [`LuaShape::Input`] with every field optional: a partial-update
    /// payload.
    Partial,
    /// What a read returns: a reference its id or — at the default read
    /// depth — the populated document, a JSON rich text value the parsed
    /// document. Every field is optional (a draft may lack required values,
    /// and field read access and `select` leave keys out).
    Read,
}

impl LuaShape {
    fn is_read(self) -> bool {
        matches!(self, LuaShape::Read)
    }

    fn all_optional(self) -> bool {
        !matches!(self, LuaShape::Input)
    }

    /// The class namespaces an array row and a group of this shape are
    /// declared under.
    pub(super) fn sub_type_namespaces(self) -> (&'static str, &'static str) {
        if self.is_read() {
            ("doc_row", "doc_group")
        } else {
            ("array_row", "group")
        }
    }
}

/// Write a single field's type definition in `shape`. Layout-only wrappers
/// promote their children transparently.
pub(super) fn write_field(
    out: &mut String,
    field: &FieldDefinition,
    parent_pascal: &str,
    shape: LuaShape,
) {
    // Layout wrappers are transparent — emit their sub-fields at this level.
    // The canonical wrapper-flatten (`flatten_array_sub_fields`) owns the
    // promotion rules.
    if field.field_type.is_layout_wrapper() {
        for sub in flatten_array_sub_fields(from_ref(field)) {
            write_field(out, sub, parent_pascal, shape);
        }
        return;
    }

    // Emit a comment for polymorphic relationships listing target collections
    if field.field_type == FieldType::Relationship
        && let Some(rc) = &field.relationship
        && rc.is_polymorphic()
    {
        let targets = rc.all_collections().join(", ");
        w!(out, "--- Polymorphic relationship — targets: {}", targets);
    }

    let lua_type = field_to_lua_type(field, parent_pascal, shape);
    let opt = if shape.all_optional() || is_optional(field) {
        "?"
    } else {
        ""
    };
    w!(
        out,
        "---@field {}{opt} {lua_type}",
        lua_field_key(&field.name)
    );

    // Each companion (a date's `{name}_tz` zone, a code field's `{name}_lang`
    // language pick) travels as an optional string key beside the value.
    for column in field.companion_columns(&field.name) {
        w!(out, "---@field {}? string", lua_field_key(&column));
    }
}

/// Map a field definition to its Lua type string in `shape`.
pub(super) fn field_to_lua_type(
    field: &FieldDefinition,
    parent_pascal: &str,
    shape: LuaShape,
) -> String {
    match &field.field_type {
        FieldType::Text => list_or_single("string", field.has_many),
        // A JSON rich text document reads parsed; a write takes the document
        // or its JSON text.
        FieldType::Richtext if field.parses_json() => json_richtext_ty(shape).to_string(),
        FieldType::Textarea
        | FieldType::Email
        | FieldType::Date
        | FieldType::Richtext
        | FieldType::Code => "string".to_string(),
        FieldType::Number => list_or_single("number", field.has_many),
        FieldType::Checkbox => "boolean".to_string(),
        FieldType::Json => "any".to_string(),
        FieldType::Select | FieldType::Radio => select_ty(field),
        FieldType::Relationship | FieldType::Upload => reference_ty(field, shape),
        FieldType::Array | FieldType::Group => sub_type_ref(field, parent_pascal, shape),
        // Layout-only; sub-fields are promoted
        FieldType::Row | FieldType::Collapsible | FieldType::Tabs => "table".to_string(),
        FieldType::Blocks | FieldType::Join => "table[]".to_string(),
    }
}

/// A JSON rich text value: a read returns the parsed document, a write takes
/// the document or its JSON text.
fn json_richtext_ty(shape: LuaShape) -> &'static str {
    if shape.is_read() {
        "table"
    } else {
        "string|table"
    }
}

/// `base`, or a list of it.
fn list_or_single(base: &str, many: bool) -> String {
    if many {
        format!("{base}[]")
    } else {
        base.to_string()
    }
}

/// A select or radio: the union of its option values, or a plain string
/// without options. A has-many one with options also admits any string.
fn select_ty(field: &FieldDefinition) -> String {
    if field.options.is_empty() {
        return list_or_single("string", field.has_many);
    }

    let base = field
        .options
        .iter()
        .map(|o| format!("\"{}\"", escape_str(&o.value)))
        .collect::<Vec<_>>()
        .join(" | ");

    if field.has_many {
        format!("({base}|string)[]")
    } else {
        base
    }
}

/// A relationship or upload. Written — and held in hook data — as its id. A
/// read at the default depth populates it, so a read types it as the id or
/// one of the target documents.
fn reference_ty(field: &FieldDefinition, shape: LuaShape) -> String {
    let many = rel_has_many(field);

    let Some(rc) = field.relationship.as_ref().filter(|_| shape.is_read()) else {
        return list_or_single("string", many);
    };

    let docs: Vec<String> = rc
        .all_collections()
        .into_iter()
        .filter(|target| !target.is_empty())
        .map(|target| format!("crap.doc.{}", to_pascal_case(target)))
        .collect();

    if docs.is_empty() {
        return list_or_single("string", many);
    }

    let one = format!("string|{}", docs.join("|"));

    if many { format!("({one})[]") } else { one }
}

/// An array's row class list or a group's class, named in `shape`'s
/// namespace. An empty-`fields` array or group declares no class (the
/// sub-type collector skips it), so referencing one would dangle — it falls
/// back to a plain `table[]` / `table`.
fn sub_type_ref(field: &FieldDefinition, parent_pascal: &str, shape: LuaShape) -> String {
    let is_array = field.field_type.has_rows();

    if field.fields.is_empty() {
        return list_or_single("table", is_array);
    }

    let (row, group) = shape.sub_type_namespaces();
    let sub = format!("{parent_pascal}{}", to_pascal_case(&field.name));

    if is_array {
        format!("crap.{row}.{sub}[]")
    } else {
        format!("crap.{group}.{sub}")
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{checkbox_field, select_field, text_field};
    use super::*;
    use crate::core::{FieldAdmin, LocalizedString, RelationshipConfig, SelectOption};

    #[test]
    fn field_type_mapping() {
        let f = text_field("x", true);
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "string");

        let mut f = text_field("x", true);
        f.field_type = FieldType::Number;
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "number");

        let f = checkbox_field("x");
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "boolean");

        let mut f = text_field("x", true);
        f.field_type = FieldType::Json;
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "any");
    }

    #[test]
    fn select_with_options() {
        let f = select_field("status", true, &["draft", "published"]);
        assert_eq!(
            field_to_lua_type(&f, "Test", LuaShape::Input),
            "\"draft\" | \"published\""
        );
    }

    #[test]
    fn select_without_options() {
        let f = select_field("status", true, &[]);
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "string");
    }

    #[test]
    fn optional_logic() {
        assert!(!is_optional(&text_field("x", true)));
        assert!(is_optional(&text_field("x", false)));
        let mut cb = checkbox_field("x");
        cb.required = true;
        assert!(is_optional(&cb));
    }

    #[test]
    fn lua_relationship_has_many() {
        let f = FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build();
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "string[]");
    }

    #[test]
    fn lua_polymorphic_has_one_type() {
        let mut rc = RelationshipConfig::new("posts", false);
        rc.polymorphic = vec!["posts".into(), "pages".into()];
        let f = FieldDefinition::builder("subject", FieldType::Relationship)
            .required(true)
            .relationship(rc)
            .build();
        // Polymorphic has-one stores "collection/id" composite as string
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "string");
    }

    #[test]
    fn lua_polymorphic_has_many_type() {
        let mut rc = RelationshipConfig::new("articles", true);
        rc.polymorphic = vec!["articles".into(), "videos".into()];
        let f = FieldDefinition::builder("related", FieldType::Relationship)
            .relationship(rc)
            .build();
        // Polymorphic has-many stores array of "collection/id" composites
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "string[]");
    }

    #[test]
    fn lua_relationship_has_one() {
        let f = FieldDefinition::builder("author", FieldType::Relationship)
            .required(true)
            .relationship(RelationshipConfig::new("users", false))
            .build();
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "string");
    }

    #[test]
    fn lua_array_type() {
        let f = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![text_field("label", true)])
            .build();
        assert_eq!(
            field_to_lua_type(&f, "Test", LuaShape::Input),
            "crap.array_row.TestItems[]"
        );
    }

    #[test]
    fn lua_array_type_empty_falls_back_to_table() {
        // No sub-fields → no `crap.array_row.*` class is declared, so the type
        // must not reference one (which would be undefined in LuaLS).
        let f = FieldDefinition::builder("items", FieldType::Array).build();
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "table[]");
    }

    #[test]
    fn lua_group_type_empty() {
        let f = FieldDefinition::builder("seo", FieldType::Group).build();
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "table");
    }

    #[test]
    fn lua_group_type_with_subfields() {
        let f = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                text_field("title", true),
                text_field("description", false),
            ])
            .build();
        assert_eq!(
            field_to_lua_type(&f, "Test", LuaShape::Input),
            "crap.group.TestSeo"
        );
    }

    #[test]
    fn lua_upload_type() {
        let f = FieldDefinition::builder("image", FieldType::Upload).build();
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "string");
    }

    #[test]
    fn upload_has_many_generates_array_type() {
        let f = FieldDefinition::builder("images", FieldType::Upload)
            .relationship(RelationshipConfig::new("", true))
            .build();
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "string[]");
    }

    #[test]
    fn lua_blocks_type() {
        let f = FieldDefinition::builder("content", FieldType::Blocks).build();
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "table[]");
    }

    #[test]
    fn lua_text_has_many() {
        let f = FieldDefinition::builder("tags", FieldType::Text)
            .has_many(true)
            .build();
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "string[]");
    }

    #[test]
    fn lua_number_has_many() {
        let f = FieldDefinition::builder("scores", FieldType::Number)
            .has_many(true)
            .build();
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "number[]");
    }

    #[test]
    fn lua_email_type() {
        let f = FieldDefinition::builder("email", FieldType::Email).build();
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "string");
    }

    #[test]
    fn lua_date_type() {
        let f = FieldDefinition::builder("at", FieldType::Date).build();
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "string");
    }

    #[test]
    fn lua_richtext_type() {
        let f = FieldDefinition::builder("body", FieldType::Richtext).build();
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "string");
    }

    #[test]
    fn lua_textarea_type() {
        let f = FieldDefinition::builder("notes", FieldType::Textarea).build();
        assert_eq!(field_to_lua_type(&f, "Test", LuaShape::Input), "string");
    }

    #[test]
    fn lua_code_join_radio_fields() {
        // Code maps to string, Join maps to table[], Radio maps to string/union
        let f_code = FieldDefinition::builder("snippet", FieldType::Code).build();
        let f_join = FieldDefinition::builder("refs", FieldType::Join).build();
        let f_radio = FieldDefinition::builder("color", FieldType::Radio).build();
        assert_eq!(
            field_to_lua_type(&f_code, "Test", LuaShape::Input),
            "string"
        );
        assert_eq!(
            field_to_lua_type(&f_join, "Test", LuaShape::Input),
            "table[]"
        );
        assert_eq!(
            field_to_lua_type(&f_radio, "Test", LuaShape::Input),
            "string"
        );
    }

    #[test]
    fn lua_select_has_many_with_and_without_options() {
        // has_many with options → (opt1 | opt2|string)[]
        let f_with_opts = FieldDefinition::builder("tags", FieldType::Select)
            .has_many(true)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("A".into()), "a"),
                SelectOption::new(LocalizedString::Plain("B".into()), "b"),
            ])
            .build();
        let result = field_to_lua_type(&f_with_opts, "Test", LuaShape::Input);
        assert!(
            result.contains("\"a\""),
            "should include option 'a': {result}"
        );
        assert!(
            result.contains("\"b\""),
            "should include option 'b': {result}"
        );
        assert!(result.ends_with("[]"), "should be an array type: {result}");

        // has_many without options → string[]
        let f_no_opts = FieldDefinition::builder("cats", FieldType::Select)
            .has_many(true)
            .build();
        assert_eq!(
            field_to_lua_type(&f_no_opts, "Test", LuaShape::Input),
            "string[]"
        );
    }

    fn read_ty(field: &FieldDefinition) -> String {
        field_to_lua_type(field, "Test", LuaShape::Read)
    }

    /// Regression: a read at the default depth returns a relationship or
    /// upload populated, but the read class typed it as the id string.
    #[test]
    fn read_reference_is_the_id_or_the_populated_document() {
        let author = FieldDefinition::builder("author", FieldType::Relationship)
            .relationship(RelationshipConfig::new("users", false))
            .build();
        assert_eq!(read_ty(&author), "string|crap.doc.Users");

        let tags = FieldDefinition::builder("tags", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build();
        assert_eq!(read_ty(&tags), "(string|crap.doc.Tags)[]");

        let cover = FieldDefinition::builder("cover", FieldType::Upload)
            .relationship(RelationshipConfig::new("media", false))
            .build();
        assert_eq!(read_ty(&cover), "string|crap.doc.Media");

        let mut rc = RelationshipConfig::new("posts", true);
        rc.polymorphic = vec!["posts".into(), "pages".into()];
        let related = FieldDefinition::builder("related", FieldType::Relationship)
            .relationship(rc)
            .build();
        assert_eq!(
            read_ty(&related),
            "(string|crap.doc.Posts|crap.doc.Pages)[]"
        );

        // No known target: the id string on either side.
        let bare = FieldDefinition::builder("image", FieldType::Upload).build();
        assert_eq!(read_ty(&bare), "string");
    }

    /// Regression: a JSON-format rich text value reads as the parsed
    /// document, but was typed as a string.
    #[test]
    fn json_richtext_reads_as_a_table() {
        let body = FieldDefinition::builder("body", FieldType::Richtext)
            .admin(FieldAdmin::builder().richtext_format("json").build())
            .build();

        assert_eq!(read_ty(&body), "table");
        assert_eq!(
            field_to_lua_type(&body, "Test", LuaShape::Input),
            "string|table"
        );
    }

    /// A read class references the read-shape sub-type classes; the input and
    /// partial shapes the input ones.
    #[test]
    fn sub_type_references_follow_the_shape() {
        let items = FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![text_field("label", true)])
            .build();
        let seo = FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![text_field("title", true)])
            .build();

        assert_eq!(read_ty(&items), "crap.doc_row.TestItems[]");
        assert_eq!(read_ty(&seo), "crap.doc_group.TestSeo");
        assert_eq!(
            field_to_lua_type(&items, "Test", LuaShape::Partial),
            "crap.array_row.TestItems[]"
        );
        assert_eq!(
            field_to_lua_type(&seo, "Test", LuaShape::Partial),
            "crap.group.TestSeo"
        );
    }

    /// Regression: a leading-digit field name was written bare
    /// (`---@field 2fa? string`), which `LuaLS` parses as the integer `2`
    /// followed by the name `fa`. A reserved word or a `LuaLS` scope word is
    /// quoted the same way; every form keeps its optional marker.
    #[test]
    fn non_identifier_field_names_are_quoted_keys() {
        for (name, line) in [
            ("2fa", "---@field [\"2fa\"]? string\n"),
            ("end", "---@field [\"end\"]? string\n"),
            ("private", "---@field [\"private\"]? string\n"),
        ] {
            let mut out = String::new();
            write_field(&mut out, &text_field(name, false), "Test", LuaShape::Input);
            assert_eq!(out, line);
        }

        let mut out = String::new();
        write_field(&mut out, &text_field("2fa", true), "Test", LuaShape::Input);
        assert_eq!(out, "---@field [\"2fa\"] string\n");
    }

    /// A leading-digit field's companion column is quoted like the field.
    #[test]
    fn non_identifier_companion_columns_are_quoted_keys() {
        let starts = FieldDefinition::builder("2day", FieldType::Date)
            .timezone(true)
            .build();

        let mut out = String::new();
        write_field(&mut out, &starts, "Test", LuaShape::Input);

        assert!(out.contains("---@field [\"2day_tz\"]? string"), "{out}");
    }

    #[test]
    fn partial_and_read_shapes_make_every_field_optional() {
        let title = text_field("title", true);

        for (shape, line) in [
            (LuaShape::Input, "---@field title string\n"),
            (LuaShape::Partial, "---@field title? string\n"),
            (LuaShape::Read, "---@field title? string\n"),
        ] {
            let mut out = String::new();
            write_field(&mut out, &title, "Test", shape);
            assert_eq!(out, line);
        }
    }
}
