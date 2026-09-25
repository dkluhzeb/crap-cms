use super::*;
use crate::core::{
    FieldAdmin, FieldType, LocalizedString, RelationshipConfig, SelectOption,
    upload::{CollectionUpload, ImageSizeBuilder},
};

fn text_field(name: &str, required: bool) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Text)
        .required(required)
        .build()
}

fn make_col(slug: &str, fields: Vec<FieldDefinition>) -> CollectionDefinition {
    let mut def = CollectionDefinition::new(slug);
    def.timestamps = true;
    def.fields = fields;
    def
}

/// Regression: a collection slugged `document` built a struct the
/// decoder's `crate::proto::Document` import shadows, so its `impl
/// FromDocument` targeted the proto message. The struct is `Document_` in
/// the client and the decoder alike.
#[test]
fn proto_decoder_never_targets_its_own_imports() {
    let mut registry = Registry::new();
    registry.register_collection(make_col("document", vec![text_field("name", false)]));
    registry.register_collection(make_col("data_map", vec![text_field("name", false)]));

    let out = render(&registry, "crate::proto");

    assert!(out.contains("impl FromDocument for Document_ {"), "{out}");
    assert!(out.contains("impl FromDocument for DataMap_ {"), "{out}");
    assert!(!out.contains("impl FromDocument for Document {"), "{out}");
}

/// Regression: a sub-type of an owner whose name the generated files already
/// bind (`box` → `Box_`) was named from the sanitized owner (`Box_Meta`),
/// while the client names it from the raw one (`BoxMeta`), so the decoder
/// built a struct the client never declares. Collection and global alike.
#[test]
fn proto_sub_types_of_a_reserved_owner_match_the_client_names() {
    let meta = || {
        FieldDefinition::builder("meta", FieldType::Group)
            .fields(vec![text_field("note", false)])
            .build()
    };

    let mut col_out = String::new();
    render_collection_impl(&mut col_out, &make_col("box", vec![meta()]));

    let mut global = GlobalDefinition::new("vec");
    global.fields = vec![meta()];
    let mut global_out = String::new();
    render_global_impl(&mut global_out, &global);

    for (out, owner, sub) in [
        (&col_out, "Box_", "BoxMeta"),
        (&global_out, "Vec_", "VecMeta"),
    ] {
        assert!(
            out.contains(&format!("impl FromDocument for {owner} {{")),
            "{out}"
        );
        assert!(out.contains(&format!("impl {sub} {{")), "{out}");
        assert!(out.contains(&format!("{sub}::from_struct(")), "{out}");
        assert!(!out.contains(&format!("{owner}Meta")), "{out}");
    }
}

/// Regression: a layout wrapper (Row/Collapsible/Tabs) nested INSIDE an
/// array sub-type used to reach `resolve_ty` unflattened and panic with
/// "layout wrappers are flattened before type resolution". The example
/// project's collections hit this on `typegen proto`. The wrapped
/// sub-field must be promoted into the sub-type's `from_struct`.
#[test]
fn proto_sub_type_flattens_layout_wrappers_inside_arrays() {
    let mut row = FieldDefinition::builder("stats_row", FieldType::Row).build();
    row.fields = vec![text_field("label", false)];

    let mut items = FieldDefinition::builder("items", FieldType::Array).build();
    items.fields = vec![text_field("title", true), row];

    let col = make_col("projects", vec![items]);
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(
        out.contains("label: s.fields.get(\"label\")"),
        "the Row-wrapped sub-field must appear in from_struct: {out}"
    );
}

/// A relational array row's `from_struct` reads its junction `id`; a row
/// nested inside another row is JSON without one.
#[test]
fn proto_array_rows_read_their_id() {
    let mut notes = FieldDefinition::builder("notes", FieldType::Array).build();
    notes.fields = vec![text_field("body", false)];
    let mut items = FieldDefinition::builder("items", FieldType::Array).build();
    items.fields = vec![text_field("title", true), notes];

    let col = make_col("projects", vec![items]);
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    let impl_of = |name: &str| {
        let start = out.find(&format!("impl {name} {{")).expect(name);
        let rest = &out[start..];
        rest[..rest.find("\n}\n").unwrap_or(rest.len())].to_string()
    };

    assert!(
        impl_of("ProjectsItems").contains("id: s.fields.get(\"id\")"),
        "{out}"
    );
    assert!(
        !impl_of("ProjectsItemsNotes").contains("id: s.fields.get(\"id\")"),
        "{out}"
    );
}

/// Identifier safety: the `from_document` struct-field position must be the
/// SANITIZED ident (matching the generated struct), while the wire lookup
/// keeps the raw key. A keyword field `type` → `r#type: get_str(doc, "type")`.
#[test]
fn proto_field_assignment_uses_sanitized_ident() {
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("type", FieldType::Text)
                .required(true)
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(
        out.contains("r#type: get_str_opt(doc, \"type\")"),
        "struct field must be sanitized while the lookup uses the raw key: {out}"
    );
}

/// Regression: a timezone date's `<name>_tz` companion was assigned under
/// its raw key, so a digit-leading field (`2fa_tz`) generated Rust that
/// doesn't compile. The struct field is sanitized; the lookup keeps the key.
#[test]
fn proto_timezone_companion_uses_a_sanitized_ident() {
    let col = make_col(
        "events",
        vec![
            FieldDefinition::builder("2fa", FieldType::Date)
                .timezone(true)
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    let ident = idents::rust_field("2fa_tz").ident;
    assert_ne!(ident, "2fa_tz");
    assert!(
        out.contains(&format!("{ident}: get_str_opt(doc, \"2fa_tz\")")),
        "{out}"
    );
}

/// A code field with a language allow-list.
fn code_with_languages(name: &str) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Code)
        .admin(
            FieldAdmin::builder()
                .languages(vec!["python".to_string()])
                .build(),
        )
        .build()
}

/// A code field's `<name>_lang` companion is decoded under a sanitized
/// ident, the lookup keeping the raw key.
#[test]
fn proto_language_companion_uses_a_sanitized_ident() {
    let col = make_col("snippets", vec![code_with_languages("2fa")]);
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    let ident = idents::rust_field("2fa_lang").ident;
    assert_ne!(ident, "2fa_lang");
    assert!(
        out.contains(&format!("{ident}: get_str_opt(doc, \"2fa_lang\")")),
        "{out}"
    );
}

/// An array row's `from_struct` decodes a code sub-field's `<name>_lang`
/// companion; a code field without an allow-list has none.
#[test]
fn proto_array_row_decodes_the_language_companion() {
    let items = FieldDefinition::builder("items", FieldType::Array)
        .fields(vec![
            code_with_languages("example"),
            FieldDefinition::builder("plain", FieldType::Code).build(),
        ])
        .build();
    let col = make_col("snippets", vec![items]);
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(
        out.contains("example_lang: s.fields.get(\"example_lang\")"),
        "{out}"
    );
    assert!(!out.contains("plain_lang"), "{out}");
}

#[test]
fn proto_collection_output() {
    let col = make_col(
        "posts",
        vec![text_field("title", true), text_field("content", false)],
    );
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(out.contains("impl Posts {"));
    assert!(out.contains("fn from_document(doc: &Document) -> Self"));
    assert!(out.contains("id: doc.id.clone()"));
    assert!(out.contains("title: get_str_opt(doc, \"title\")"));
    assert!(out.contains("content: get_str_opt(doc, \"content\")"));
    assert!(out.contains("created_at: doc.created_at.clone()"));
}

/// Alignment with `client/rust.rs`: a select/radio WITH options decodes
/// through the generated enum's `From<String>` (the client field is the enum).
#[test]
fn proto_select_with_options_wraps_in_enum() {
    let col = make_col(
        "tasks",
        vec![
            FieldDefinition::builder("status", FieldType::Select)
                .required(true)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Todo".into()),
                    "todo",
                )])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection_impl(&mut out, &col);
    assert!(
        out.contains("status: get_str_opt(doc, \"status\").map(TasksStatus::from)"),
        "select→enum: {out}"
    );
}

/// A required single relationship is `Option<Rel<T>>` on the client (populate
/// can null it), so proto must NOT unwrap it.
#[test]
fn proto_single_relationship_is_optional() {
    let col = make_col(
        "comments",
        vec![
            FieldDefinition::builder("author", FieldType::Relationship)
                .required(true)
                .relationship(RelationshipConfig::new("users", false))
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection_impl(&mut out, &col);
    assert!(out.contains("author: get_rel(doc, \"author\"),"), "{out}");
    assert!(!out.contains("unwrap_or(Rel::Id"), "no unwrap: {out}");
}

/// A relationship nested inside a group/array decodes BOTH forms — the id
/// string and the populated document (the gap the old sub-field path missed).
#[test]
fn proto_nested_relationship_decodes_populated_doc() {
    let author = FieldDefinition::builder("author", FieldType::Relationship)
        .relationship(RelationshipConfig::new("users", false))
        .build();
    let group = FieldDefinition::builder("meta", FieldType::Group)
        .fields(vec![author])
        .build();
    let col = make_col("posts", vec![group]);
    let mut out = String::new();
    render_collection_impl(&mut out, &col);
    assert!(
        out.contains("Rel::Doc(Box::new(Users::from_document(&struct_to_document(m))))"),
        "nested relationship decodes the populated document: {out}"
    );
}

/// A polymorphic relationship decodes into the generated discriminated enum —
/// id string → `Id`, `{collection, ...}` struct → the matching doc variant.
#[test]
fn proto_polymorphic_decodes_discriminated_enum() {
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
    render_collection_impl(&mut out, &col);
    assert!(
        out.contains("CommentsSubject::Id(s.clone())"),
        "id form: {out}"
    );
    assert!(
            out.contains(
                "Some(\"posts\") => Some(CommentsSubject::Doc(CommentsSubjectRef::Posts(Box::new(Posts::from_document"
            ),
            "populated variant: {out}"
        );
    assert!(
        out.contains("struct_to_document(m)"),
        "reads the struct: {out}"
    );
}

#[test]
fn proto_number_and_bool_fields() {
    let col = make_col(
        "items",
        vec![
            FieldDefinition::builder("price", FieldType::Number)
                .required(true)
                .build(),
            FieldDefinition::builder("active", FieldType::Checkbox).build(),
        ],
    );
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(out.contains("price: get_num_opt(doc, \"price\")"));
    assert!(out.contains("active: get_bool_opt(doc, \"active\")"));
}

#[test]
fn proto_relationship_fields() {
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("author", FieldType::Relationship)
                .required(true)
                .relationship(RelationshipConfig::new("users", false))
                .build(),
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(
        out.contains("author: get_rel(doc, \"author\")"),
        "required has-one should use get_rel: {out}"
    );
    assert!(
        out.contains("get_rel_list(doc, \"tags\")"),
        "optional has-many should use get_rel_list: {out}"
    );
}

/// Regression: a required relationship SUB-field (inside an array row) must
/// decode to `Rel::Id`, not `Default::default()` — `Rel` has no `Default`, so
/// the old fallback produced non-compiling client code (E0277).
#[test]
fn proto_sub_field_relationship_uses_rel_not_default() {
    let product = FieldDefinition::builder("product", FieldType::Relationship)
        .required(true)
        .relationship(RelationshipConfig::new("products", false))
        .build();
    let items = FieldDefinition::builder("items", FieldType::Array)
        .fields(vec![text_field("label", true), product])
        .build();
    let col = make_col("orders", vec![items]);
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(out.contains("impl OrdersItems {"), "sub-type impl: {out}");
    assert!(
        out.contains("Some(Kind::StringValue(s)) if !s.is_empty() => Some(Rel::Id(s.clone()))"),
        "relationship sub-field must decode to Rel::Id: {out}"
    );
    assert!(
        !out.contains("product: Default::default()"),
        "relationship must not fall back to Default: {out}"
    );
}

/// Regression: a `has_many` NUMBER sub-field must decode as a numeric list
/// (`Vec<f64>`), not a string list — the old code routed it through
/// `get_str_list`, an E0308 against the `Vec<f64>` struct field.
#[test]
fn proto_sub_field_has_many_number_decodes_numeric() {
    let mut scores = FieldDefinition::builder("scores", FieldType::Number).build();
    scores.has_many = true;
    let items = FieldDefinition::builder("items", FieldType::Array)
        .fields(vec![scores])
        .build();
    let col = make_col("games", vec![items]);
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(
        out.contains("impl GamesItems {"),
        "compound sub-type: {out}"
    );
    assert!(
        out.contains("Some(Kind::IntValue(n)) => Some(*n as f64)"),
        "has-many number sub-field must decode numerically: {out}"
    );
}

/// Regression: a group nested inside an array row uses the COMPOUND sub-type
/// name and decodes via that sub-type's `from_struct` — previously it fell
/// back to `Default::default()` (silent empty / E0277).
#[test]
fn proto_nested_group_in_array_uses_compound_from_struct() {
    let meta = FieldDefinition::builder("meta", FieldType::Group)
        .fields(vec![text_field("author", true)])
        .build();
    let items = FieldDefinition::builder("items", FieldType::Array)
        .fields(vec![meta])
        .build();
    let col = make_col("posts", vec![items]);
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(
        out.contains("impl PostsItemsMeta {"),
        "nested group must get a compound sub-type: {out}"
    );
    assert!(
        out.contains("PostsItemsMeta::from_struct"),
        "nested group must decode via from_struct: {out}"
    );
}

/// Regression (compound-path collision): a top-level group AND a same-named
/// group nested in an array must produce DISTINCT sub-types, not two `impl
/// PostsSeo` (which redeclares/collides in the generated client).
#[test]
fn proto_same_name_group_at_two_depths_no_collision() {
    let nested_seo = FieldDefinition::builder("seo", FieldType::Group)
        .fields(vec![text_field("keyword", true)])
        .build();
    let variants = FieldDefinition::builder("variants", FieldType::Array)
        .fields(vec![nested_seo])
        .build();
    let top_seo = FieldDefinition::builder("seo", FieldType::Group)
        .fields(vec![text_field("title", true)])
        .build();
    let col = make_col("posts", vec![top_seo, variants]);
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(out.contains("impl PostsSeo {"), "top-level: {out}");
    assert!(
        out.contains("impl PostsVariantsSeo {"),
        "nested must be compound-named, not a second PostsSeo: {out}"
    );
}

#[test]
fn proto_array_with_subfields() {
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    text_field("label", true),
                    FieldDefinition::builder("done", FieldType::Checkbox).build(),
                ])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(
        out.contains("impl PostsItems {"),
        "should generate sub-type impl: {out}"
    );
    assert!(
        out.contains("fn from_struct(s: &DataMap)"),
        "should have from_struct: {out}"
    );
    assert!(
        out.contains("PostsItems::from_struct(s)"),
        "should call from_struct in array extraction: {out}"
    );
}

#[test]
fn proto_group_calls_from_struct_not_default() {
    // Regression: a top-level Group used to fall through to
    // `Default::default()` (silent data loss) even though its `from_struct`
    // impl is generated. It must extract the nested struct.
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![text_field("title", true)])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(
        out.contains("impl PostsSeo {"),
        "should generate group sub-type impl: {out}"
    );
    assert!(
        out.contains("PostsSeo::from_struct(s)"),
        "group must extract via from_struct, not default: {out}"
    );
}

#[test]
fn proto_helpers_output() {
    let mut out = String::new();
    render_helpers(&mut out);

    assert!(out.contains("fn get_str(doc: &Document, name: &str) -> String"));
    assert!(out.contains("fn get_str_opt(doc: &Document, name: &str) -> Option<String>"));
    assert!(out.contains("fn get_num_opt(doc: &Document, name: &str) -> Option<f64>"));
    assert!(out.contains("fn get_bool_opt(doc: &Document, name: &str) -> Option<bool>"));
    // Every decoded field is optional, so no non-optional numeric or boolean
    // getter is emitted for the generated code to leave unused.
    assert!(!out.contains("fn get_num(doc"), "{out}");
    assert!(!out.contains("fn get_bool(doc"), "{out}");
    assert!(out.contains("fn get_str_list(doc: &Document, name: &str) -> Vec<String>"));
}

#[test]
fn proto_full_render() {
    let mut registry = Registry::new();
    registry.register_collection(make_col("posts", vec![text_field("title", true)]));
    let out = render(&registry, "crate::proto");

    assert!(out.contains("use crate::proto::{DataMap, Document, FieldValue};"));
    assert!(out.contains("use crate::proto::field_value::Kind;"));
    assert!(out.contains("impl Posts {"));
}

/// Regression: the generated Rust client must decode the typed `FieldValue`
/// wire format, never the removed `google.protobuf.Struct`. It references
/// `field_value::Kind` + `IntValue`/`DoubleValue`/`DataMap`, and must NOT
/// emit `prost_types` or the removed `NumberValue` variant.
#[test]
fn proto_uses_typed_field_value_not_struct() {
    let mut registry = Registry::new();
    registry.register_collection(make_col(
        "items",
        vec![
            FieldDefinition::builder("price", FieldType::Number)
                .required(true)
                .build(),
        ],
    ));
    let out = render(&registry, "crate::proto");

    assert!(
        out.contains("use crate::proto::field_value::Kind;"),
        "{out}"
    );
    assert!(
        out.contains("use crate::proto::{DataMap, Document, FieldValue};"),
        "{out}"
    );
    assert!(
        out.contains("Some(Kind::IntValue(n)) => Some(*n as f64)"),
        "{out}"
    );
    assert!(
        out.contains("Some(Kind::DoubleValue(n)) => Some(*n)"),
        "{out}"
    );
    assert!(out.contains("fn struct_to_document(s: &DataMap)"), "{out}");

    assert!(
        !out.contains("prost_types"),
        "generated client must not reference prost_types: {out}"
    );
    assert!(
        !out.contains("NumberValue"),
        "generated client must not reference the removed NumberValue: {out}"
    );
}

#[test]
fn proto_row_promotes_subfields() {
    let col = make_col(
        "items",
        vec![
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![text_field("first", true), text_field("last", false)])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(
        out.contains("first: get_str_opt(doc, \"first\")"),
        "row sub-fields promoted: {out}"
    );
    assert!(
        out.contains("last: get_str_opt(doc, \"last\")"),
        "row sub-fields promoted: {out}"
    );
    assert!(
        !out.contains("row:"),
        "row field itself should not appear: {out}"
    );
}

/// A rich text field stored as a JSON document decodes to the document.
/// Matching only `StringValue` (what the string typing produced) dropped
/// the object the encoder sends, silently decoding the field to `None`.
#[test]
fn json_rich_text_decodes_the_document_and_html_stays_a_string() {
    let json_body = FieldDefinition::builder("body", FieldType::Richtext)
        .admin(FieldAdmin::builder().richtext_format("json").build())
        .build();
    let html_body = FieldDefinition::builder("teaser", FieldType::Richtext).build();

    let col = make_col("pages", vec![json_body, html_body]);
    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(out.contains("body: get_json(doc, \"body\")"), "{out}");
    assert!(
        out.contains("teaser: get_str_opt(doc, \"teaser\")"),
        "{out}"
    );
}

/// The emitted JSON helper accepts the structured wire forms, not just
/// scalars — an object arrives as `StructValue`, an array as `ListValue`.
#[test]
fn the_json_helper_accepts_structured_wire_values() {
    let mut reg = Registry::new();
    reg.register_collection(make_col("pages", vec![text_field("title", true)]));

    let out = render(&reg, "crate::proto");

    assert!(
        out.contains("fn field_value_to_json(v: &FieldValue)"),
        "{out}"
    );
    assert!(out.contains("Some(Kind::StructValue(s)) => serde_json::Value::Object("));
    assert!(out.contains("Some(Kind::ListValue(list)) => serde_json::Value::Array("));
    assert!(out.contains("use crate::proto::{DataMap, Document, FieldValue};"));
}

/// An upload collection's per-size columns never arrive: the read folds
/// them into one `sizes` object, so the decoder must read that instead.
#[test]
fn upload_size_columns_decode_as_the_assembled_object() {
    let mut upload = CollectionUpload::new();
    upload.image_sizes = vec![
        ImageSizeBuilder::new("thumbnail")
            .width(200)
            .height(200)
            .build(),
    ];

    let mut col = make_col("media", vec![text_field("filename", true)]);
    col.fields.extend(
        upload
            .size_columns()
            .into_iter()
            .map(|(name, ty)| FieldDefinition::builder(name, ty).build()),
    );
    col.upload = Some(upload);

    let mut out = String::new();
    render_collection_impl(&mut out, &col);

    assert!(!out.contains("thumbnail_url"), "{out}");
    assert!(!out.contains("thumbnail_width"), "{out}");
    assert!(out.contains("sizes: "), "{out}");
    assert!(out.contains("MediaSizesThumbnail::from_struct"), "{out}");
}

/// Regression: a `hidden = true` field is stripped from every read, yet
/// the decoder assigned it (and the client struct declared it) — for a
/// collection and a global alike, nested in a group too.
#[test]
fn proto_skips_hidden_fields() {
    let secret = FieldDefinition::builder("secret", FieldType::Text)
        .hidden(true)
        .build();
    let fields = vec![
        text_field("title", true),
        secret.clone(),
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![text_field("meta", false), secret])
            .build(),
    ];

    let mut out = String::new();
    render_collection_impl(&mut out, &make_col("posts", fields.clone()));
    assert!(out.contains("title: "), "{out}");
    assert!(out.contains("meta: "), "{out}");
    assert!(!out.contains("secret"), "{out}");

    let mut global = GlobalDefinition::new("settings");
    global.fields = fields;
    let mut out = String::new();
    render_global_impl(&mut out, &global);
    assert!(out.contains("title: "), "{out}");
    assert!(!out.contains("secret"), "{out}");
}
