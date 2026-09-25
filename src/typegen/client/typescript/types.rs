//! One field as a TypeScript property: its read and input spellings and the
//! type each [`FieldTy`] maps to.

use crate::typegen::{
    client::{Field, FieldTy, writer::CodeWriter},
    helpers::to_pascal_case,
    idents,
};

/// Emit one read property: optional, and `null` where the stored value is
/// empty.
pub(super) fn emit_read_field(w: &mut CodeWriter, field: &Field) {
    emit_property(w, field, true, &format!("{} | null", ts_ty(&field.ty)));
}

/// Emit one input property: its own optionality, a reference as its id, and a
/// nested sub-type as its input variant.
/// A nullable input key also takes `null`: an absent key keeps the stored
/// value, an explicit `null` clears it. A required key cannot be cleared.
pub(super) fn emit_input_field(w: &mut CodeWriter, field: &Field) {
    let ty = input_ty(&field.ty);
    let ty = if field.nullable {
        format!("{ty} | null")
    } else {
        ty
    };

    emit_property(w, field, field.optional, &ty);
}

/// Emit one interface property: an optional polymorphic-target `JSDoc` comment,
/// then `key?: type;` (`?` when `optional`).
pub(super) fn emit_property(w: &mut CodeWriter, field: &Field, optional: bool, ty: &str) {
    if let FieldTy::PolyRel { targets, .. } = &field.ty {
        w.line(&format!(
            "/** Polymorphic relationship — targets: {} */",
            targets.join(", ")
        ));
    }
    let opt = if optional { "?" } else { "" };
    w.line(&format!("{}{opt}: {ty};", idents::ts_key(&field.name)));
}

/// The TypeScript type of an input value: a sub-type is its `…Data` variant.
fn input_ty(ty: &FieldTy) -> String {
    let FieldTy::SubType { name, list } = ty else {
        return ts_ty(ty);
    };

    let n = format!("{}Data", idents::ts_type(name));
    if *list { format!("{n}[]") } else { n }
}

/// Map a [`FieldTy`] to its TypeScript type string.
pub(super) fn ts_ty(ty: &FieldTy) -> String {
    match ty {
        FieldTy::Str => "string".to_string(),
        FieldTy::Num => "number".to_string(),
        FieldTy::Bool => "boolean".to_string(),
        FieldTy::Json => "unknown".to_string(),
        FieldTy::Map => "Record<string, unknown>".to_string(),
        FieldTy::StrList => "string[]".to_string(),
        FieldTy::NumList => "number[]".to_string(),
        FieldTy::JsonList => "Record<string, unknown>[]".to_string(),
        // A typed relationship is an id string OR a populated document (the API's
        // `depth` controls which), so it unions the id with the target's
        // `Document` type. An empty-target relationship and every write-shape
        // reference (`None`) are plain id strings.
        FieldTy::Rel {
            target: Some(t),
            many,
        } => {
            let doc = format!("{}Document", idents::ts_type(t));
            if *many {
                format!("(string | {doc})[]")
            } else {
                format!("string | {doc}")
            }
        }
        FieldTy::Rel { target: None, many } => {
            if *many {
                "string[]".to_string()
            } else {
                "string".to_string()
            }
        }
        // A polymorphic relationship is an id string OR one of the target docs
        // (populated at depth>=1), so it unions the id with every target's Document.
        FieldTy::PolyRel { targets, many, .. } => {
            let mut parts = vec!["string".to_string()];
            parts.extend(
                targets
                    .iter()
                    .map(|t| format!("{}Document", idents::ts_type(&to_pascal_case(t)))),
            );
            let base = parts.join(" | ");
            if *many { format!("({base})[]") } else { base }
        }
        FieldTy::SubType { name, list } => {
            let n = idents::ts_type(name);
            if *list { format!("{n}[]") } else { n }
        }
        FieldTy::Localized(inner) => format!("Localized<{}>", ts_ty(inner)),
        FieldTy::Enum { values, many, .. } => {
            let base = literal_union(values);
            if *many { format!("({base})[]") } else { base }
        }
        FieldTy::Literal(values) => literal_union(values),
    }
}

/// The string-literal union of `values` (`"a" | "b"`).
fn literal_union(values: &[String]) -> String {
    values
        .iter()
        .map(|v| format!("\"{}\"", idents::escape_str(v)))
        .collect::<Vec<_>>()
        .join(" | ")
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{
        code_with_languages, interface_block, make_col, render_collection, select_field, text_field,
    };
    use crate::core::{
        BlockDefinition, FieldDefinition, FieldType, LocalizedString, RelationshipConfig,
        SelectOption,
    };

    /// A code field with a language allow-list carries its `<name>_lang`
    /// companion (also accepted as input), at the top level and inside a group.
    #[test]
    fn typescript_code_field_has_its_language_companion() {
        let col = make_col(
            "snippets",
            vec![
                code_with_languages("snippet"),
                FieldDefinition::builder("plain", FieldType::Code).build(),
                FieldDefinition::builder("meta", FieldType::Group)
                    .fields(vec![code_with_languages("example")])
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);

        let doc = interface_block(&out, "export interface SnippetsDocument {");
        assert!(doc.contains("  snippet_lang?: string | null;"), "{doc}");

        let data = interface_block(&out, "export interface SnippetsData {");
        assert!(data.contains("  snippet_lang?: string | null;"), "{data}");

        let meta = interface_block(&out, "export interface SnippetsMeta {");
        assert!(meta.contains("  example_lang?: string | null;"), "{meta}");

        assert!(!out.contains("plain_lang"), "{out}");
    }

    /// Identifier safety: a digit-leading slug/field, a keyword field, and a
    /// select-option value with special chars must all produce valid TS.
    #[test]
    fn typescript_sanitizes_unsafe_names_and_option_values() {
        let col = make_col(
            "2fa",
            vec![
                text_field("type", true),
                text_field("2fa", false),
                select_field("status", &["a\"b"]),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);

        assert!(
            out.contains("export interface N2faData {"),
            "leading-digit type must be prefixed: {out}"
        );
        assert!(out.contains("  type: string;"), "keyword bare key: {out}");
        assert!(
            out.contains("  \"2fa\"?: string | null;"),
            "leading-digit key quoted: {out}"
        );
        assert!(out.contains("\"a\\\"b\""), "option quote escaped: {out}");
    }

    #[test]
    fn typescript_relationship_has_many() {
        let col = make_col(
            "posts",
            vec![
                FieldDefinition::builder("tags", FieldType::Relationship)
                    .relationship(RelationshipConfig::new("tags", true))
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);

        let doc = interface_block(&out, "export interface PostsDocument {");
        assert!(
            doc.contains("tags?: (string | TagsDocument)[] | null;"),
            "{doc}"
        );

        let data = interface_block(&out, "export interface PostsData {");
        assert!(data.contains("  tags?: string[] | null;"), "{data}");
    }

    #[test]
    fn typescript_polymorphic_has_one() {
        let mut rc = RelationshipConfig::new("posts", false);
        rc.polymorphic = vec!["posts".into(), "pages".into()];
        let col = make_col(
            "comments",
            vec![
                FieldDefinition::builder("subject", FieldType::Relationship)
                    .required(true)
                    .relationship(rc)
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);

        // Poly has-one reads as the id string OR any target doc, optional.
        let doc = interface_block(&out, "export interface CommentsDocument {");
        assert!(
            doc.contains("  subject?: string | PostsDocument | PagesDocument | null;"),
            "poly has-one union: {doc}"
        );
        assert!(doc.contains("Polymorphic relationship"), "comment: {doc}");

        // A write sends the required `"collection/id"` string.
        let data = interface_block(&out, "export interface CommentsData {");
        assert!(data.contains("  subject: string;"), "{data}");
    }

    #[test]
    fn typescript_polymorphic_has_many() {
        let mut rc = RelationshipConfig::new("articles", true);
        rc.polymorphic = vec!["articles".into(), "videos".into()];
        let col = make_col(
            "posts",
            vec![
                FieldDefinition::builder("related", FieldType::Relationship)
                    .has_many(true)
                    .relationship(rc)
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);

        let doc = interface_block(&out, "export interface PostsDocument {");
        assert!(
            doc.contains("  related?: (string | ArticlesDocument | VideosDocument)[] | null;"),
            "poly has-many union array: {doc}"
        );
        assert!(doc.contains("Polymorphic relationship"), "comment: {doc}");

        let data = interface_block(&out, "export interface PostsData {");
        assert!(data.contains("  related?: string[] | null;"), "{data}");
    }

    #[test]
    fn typescript_relationship_has_one() {
        let col = make_col(
            "posts",
            vec![
                FieldDefinition::builder("author", FieldType::Relationship)
                    .required(true)
                    .relationship(RelationshipConfig::new("users", false))
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);

        // Single relationship is optional on read (populate can null it).
        let doc = interface_block(&out, "export interface PostsDocument {");
        assert!(
            doc.contains("  author?: string | UsersDocument | null;"),
            "{doc}"
        );
    }

    /// Regression: the create input typed a required single relationship
    /// optional (the read-side "population may null it" rule leaked into the
    /// write type) and accepted a populated document, which every write
    /// surface rejects — a write carries the id.
    #[test]
    fn typescript_input_reference_is_a_required_id() {
        let col = make_col(
            "posts",
            vec![
                FieldDefinition::builder("author", FieldType::Relationship)
                    .required(true)
                    .relationship(RelationshipConfig::new("users", false))
                    .build(),
                FieldDefinition::builder("cover", FieldType::Upload)
                    .relationship(RelationshipConfig::new("media", false))
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);

        let data = interface_block(&out, "export interface PostsData {");
        assert!(data.contains("  author: string;"), "{data}");
        assert!(data.contains("  cover?: string | null;"), "{data}");
        assert!(!data.contains("Document"), "{data}");
    }

    #[test]
    fn typescript_number_checkbox_json_fields() {
        let col = make_col(
            "items",
            vec![
                FieldDefinition::builder("price", FieldType::Number)
                    .required(true)
                    .build(),
                FieldDefinition::builder("active", FieldType::Checkbox).build(),
                FieldDefinition::builder("meta", FieldType::Json).build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);
        assert!(out.contains("  price: number;"));
        assert!(out.contains("  active?: boolean | null;"));
        assert!(out.contains("  meta?: unknown | null;"));
    }

    #[test]
    fn typescript_blocks_field() {
        let mut bd = BlockDefinition::new("text", vec![text_field("body", true)]);
        bd.label = Some(LocalizedString::Plain("Text".to_string()));
        let col = make_col(
            "pages",
            vec![
                FieldDefinition::builder("content", FieldType::Blocks)
                    .blocks(vec![bd])
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);
        assert!(out.contains("Record<string, unknown>[]"));
    }

    #[test]
    fn typescript_upload_field() {
        let col = make_col(
            "items",
            vec![
                FieldDefinition::builder("image", FieldType::Upload)
                    .required(true)
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);

        // Single upload is optional on read, required as input.
        let doc = interface_block(&out, "export interface ItemsDocument {");
        assert!(doc.contains("  image?: string | null;"), "{doc}");
        let data = interface_block(&out, "export interface ItemsData {");
        assert!(data.contains("  image: string;"), "{data}");
    }

    #[test]
    fn upload_has_many_generates_array_type() {
        let col = make_col(
            "items",
            vec![
                FieldDefinition::builder("images", FieldType::Upload)
                    .required(true)
                    .relationship(RelationshipConfig::new("", true))
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);
        assert!(
            out.contains("  images: string[];"),
            "has-many upload: {out}"
        );
    }

    #[test]
    fn typescript_select_without_options() {
        let col = make_col(
            "items",
            vec![
                FieldDefinition::builder("category", FieldType::Select)
                    .required(true)
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);
        assert!(out.contains("  category: string;"));
    }

    #[test]
    fn typescript_text_has_many() {
        let col = make_col(
            "items",
            vec![
                FieldDefinition::builder("tags", FieldType::Text)
                    .has_many(true)
                    .required(true)
                    .build(),
                FieldDefinition::builder("labels", FieldType::Text)
                    .has_many(true)
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);
        assert!(out.contains("  tags: string[];"), "required: {out}");
        assert!(
            out.contains("  labels?: string[] | null;"),
            "optional: {out}"
        );
    }

    #[test]
    fn typescript_number_has_many() {
        let col = make_col(
            "items",
            vec![
                FieldDefinition::builder("scores", FieldType::Number)
                    .has_many(true)
                    .required(true)
                    .build(),
                FieldDefinition::builder("weights", FieldType::Number)
                    .has_many(true)
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);
        assert!(out.contains("  scores: number[];"), "required: {out}");
        assert!(
            out.contains("  weights?: number[] | null;"),
            "optional: {out}"
        );
    }

    #[test]
    fn typescript_email_date_richtext_textarea() {
        let col = make_col(
            "items",
            vec![
                FieldDefinition::builder("contact", FieldType::Email)
                    .required(true)
                    .build(),
                FieldDefinition::builder("at", FieldType::Date)
                    .required(true)
                    .build(),
                FieldDefinition::builder("body", FieldType::Richtext)
                    .required(true)
                    .build(),
                FieldDefinition::builder("notes", FieldType::Textarea)
                    .required(true)
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);
        assert!(out.contains("  contact: string;"));
        assert!(out.contains("  at: string;"));
        assert!(out.contains("  body: string;"));
        assert!(out.contains("  notes: string;"));
    }

    #[test]
    fn typescript_code_join_radio_fields() {
        let col = make_col(
            "items",
            vec![
                FieldDefinition::builder("snippet", FieldType::Code)
                    .required(true)
                    .build(),
                FieldDefinition::builder("refs", FieldType::Join).build(),
                FieldDefinition::builder("color", FieldType::Radio)
                    .required(true)
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);
        assert!(out.contains("  snippet: string;"), "code → string: {out}");
        assert!(out.contains("  color: string;"), "radio no-options: {out}");

        // A join is read, never written.
        let doc = interface_block(&out, "export interface ItemsDocument {");
        assert!(
            doc.contains("  refs?: Record<string, unknown>[] | null;"),
            "join → Record[]: {doc}"
        );
        let data = interface_block(&out, "export interface ItemsData {");
        assert!(!data.contains("refs"), "{data}");
    }

    #[test]
    fn typescript_select_has_many_with_options() {
        let col = make_col(
            "items",
            vec![
                FieldDefinition::builder("tags", FieldType::Select)
                    .has_many(true)
                    .required(true)
                    .options(vec![
                        SelectOption::new(LocalizedString::Plain("A".into()), "a"),
                        SelectOption::new(LocalizedString::Plain("B".into()), "b"),
                    ])
                    .build(),
                FieldDefinition::builder("cats", FieldType::Select)
                    .has_many(true)
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);
        assert!(out.contains("(\"a\" | \"b\")[]"), "union array: {out}");
        assert!(out.contains("string[]"), "no-options → string[]: {out}");
    }

    #[test]
    fn typescript_radio_has_many_with_options() {
        let col = make_col(
            "items",
            vec![
                FieldDefinition::builder("sizes", FieldType::Radio)
                    .has_many(true)
                    .required(true)
                    .options(vec![
                        SelectOption::new(LocalizedString::Plain("S".into()), "s"),
                        SelectOption::new(LocalizedString::Plain("L".into()), "l"),
                    ])
                    .build(),
            ],
        );
        let mut out = String::new();
        render_collection(&mut out, &col);
        assert!(
            out.contains("(\"s\" | \"l\")[]"),
            "radio union array: {out}"
        );
    }
}
