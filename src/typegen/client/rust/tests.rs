use super::*;
use crate::{
    core::{
        BlockDefinition, CollectionDefinition, FieldDefinition, FieldTab, FieldType,
        GlobalDefinition, LocalizedString, RelationshipConfig, SelectOption,
    },
    typegen::client::drive,
};

fn render(registry: &Registry) -> String {
    drive(registry, Box::new(RustPrinter::new()))
}

fn render_collection(out: &mut String, col: &CollectionDefinition) {
    let mut r = Registry::new();
    r.register_collection(col.clone());
    out.push_str(&render(&r));
}

fn render_global(out: &mut String, global: &GlobalDefinition) {
    let mut r = Registry::new();
    r.register_global(global.clone());
    out.push_str(&render(&r));
}

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

/// The generated Rust must parse — a compile-grade check that closes the
/// string-match gap the raw-string generators left open.
#[test]
fn generated_rust_parses() {
    let mut registry = Registry::new();
    registry.register_collection(make_col(
        "2fa",
        vec![
            text_field("type", true),
            text_field("self", false),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![text_field("label", true)])
                .build(),
            FieldDefinition::builder("author", FieldType::Relationship)
                .required(true)
                .relationship(RelationshipConfig::new("users", false))
                .build(),
        ],
    ));
    let mut settings = GlobalDefinition::new("settings");
    settings.fields = vec![text_field("name", true)];
    registry.register_global(settings);

    let src = render(&registry);
    syn::parse_file(&src).unwrap_or_else(|e| panic!("generated Rust must parse: {e}\n---\n{src}"));
}

/// Regression: a slug `PascalCase`ing onto a name the file already binds
/// — the `Rel<T>` wrapper, the std `Option`, a `serde` import — declared a
/// clashing or shadowing struct and the output did not compile. The struct
/// is renamed and every reference follows it.
#[test]
fn prelude_names_are_not_redeclared() {
    let mut registry = Registry::new();
    for slug in ["rel", "option", "serialize", "none"] {
        registry.register_collection(make_col(slug, vec![text_field("name", false)]));
    }
    registry.register_collection(make_col(
        "posts",
        vec![
            FieldDefinition::builder("owner", FieldType::Relationship)
                .relationship(RelationshipConfig::new("rel", false))
                .build(),
        ],
    ));

    let src = render(&registry);
    for decl in [
        "pub struct Rel_ {",
        "pub struct Option_ {",
        "pub struct Serialize_ {",
        "pub struct None_ {",
        "Option<Rel<Rel_>>",
    ] {
        assert!(src.contains(decl), "{decl}: {src}");
    }
    assert!(!src.contains("pub struct Rel {"), "{src}");
    syn::parse_file(&src).unwrap_or_else(|e| panic!("generated Rust must parse: {e}\n---\n{src}"));
}

/// Identifier safety: a digit-leading slug (→ `N`-prefixed struct), a raw-able
/// keyword field (`r#type`, no rename), a non-raw keyword (`self_` + rename),
/// and a leading-digit field (`n2fa` + rename).
#[test]
fn rust_types_sanitizes_unsafe_field_names() {
    let col = make_col(
        "2fa",
        vec![
            text_field("type", true),
            text_field("self", false),
            text_field("2fa", false),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);

    assert!(
        out.contains("pub struct N2fa {"),
        "leading-digit struct prefixed: {out}"
    );
    assert!(
        out.contains("pub r#type: Option<String>,"),
        "keyword → r#type: {out}"
    );
    assert!(
        out.contains("#[serde(rename = \"self\""),
        "self renamed: {out}"
    );
    assert!(out.contains("pub self_:"), "self → self_: {out}");
    assert!(
        out.contains("#[serde(rename = \"2fa\""),
        "2fa renamed: {out}"
    );
    assert!(out.contains("pub n2fa:"), "2fa → n2fa: {out}");
}

#[test]
fn rust_collection_output() {
    let col = make_col(
        "posts",
        vec![text_field("title", true), text_field("content", false)],
    );

    let mut out = String::new();
    render_collection(&mut out, &col);

    assert!(out.contains("#[derive(Debug, Clone, Serialize, Deserialize)]"));
    assert!(out.contains("pub struct Posts {"));
    assert!(out.contains("    pub id: String,"));
    assert!(out.contains("    pub title: Option<String>,"));
    assert!(out.contains("    pub content: Option<String>,"));
    assert!(out.contains("    pub created_at: Option<String>,"));
}

#[test]
fn rust_relationship_has_many() {
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
    assert!(out.contains("Option<Vec<Rel<Tags>>>"));
}

#[test]
fn rust_polymorphic_has_one() {
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
    // Polymorphic has-one is a typed discriminated enum (id or a target doc),
    // optional on read (populate can null it).
    assert!(
        out.contains("pub subject: Option<CommentsSubject>,"),
        "poly has-one → enum: {out}"
    );
    assert!(
        out.contains("#[serde(untagged)]"),
        "wrapper is untagged: {out}"
    );
    assert!(
        out.contains("pub enum CommentsSubject {"),
        "wrapper enum: {out}"
    );
    assert!(out.contains("Id(String),"), "id variant: {out}");
    assert!(
        out.contains("#[serde(tag = \"collection\")]"),
        "doc enum tagged by collection: {out}"
    );
    assert!(
        out.contains("pub enum CommentsSubjectRef {"),
        "doc enum: {out}"
    );
    assert!(
        out.contains("Posts(Box<Posts>),") && out.contains("Pages(Box<Pages>),"),
        "per-target variants: {out}"
    );
    assert!(out.contains("Polymorphic relationship"), "comment: {out}");
}

#[test]
fn rust_polymorphic_single_target() {
    let mut rc = RelationshipConfig::new("posts", false);
    rc.polymorphic = vec!["posts".into()];
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
    assert!(out.contains("pub enum CommentsSubjectRef {"), "{out}");
    assert!(
        out.contains("Posts(Box<Posts>),"),
        "single-target poly still emits a variant: {out}"
    );
}

#[test]
fn rust_polymorphic_has_many() {
    let mut rc = RelationshipConfig::new("articles", true);
    rc.polymorphic = vec!["articles".into(), "videos".into()];
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("related", FieldType::Relationship)
                .relationship(rc)
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(
        out.contains("pub related: Option<Vec<PostsRelated>>,"),
        "poly has-many → Vec of enum: {out}"
    );
    assert!(
        out.contains("Articles(Box<Articles>),") && out.contains("Videos(Box<Videos>),"),
        "per-target variants: {out}"
    );
    assert!(out.contains("Polymorphic relationship"), "comment: {out}");
}

#[test]
fn rust_relationship_has_one_required() {
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
    // Even a required single relationship is optional on read (populate can
    // null it when the target is soft-deleted / access-denied).
    assert!(
        out.contains("pub author: Option<Rel<Users>>,"),
        "got: {out}"
    );
}

#[test]
fn rust_number_checkbox_json_fields() {
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
    assert!(out.contains("pub price: Option<f64>,"));
    assert!(out.contains("Option<bool>"));
    assert!(out.contains("Option<serde_json::Value>"));
}

#[test]
fn rust_array_with_subfields() {
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![text_field("label", true)])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("pub struct PostsItems {"));
    assert!(out.contains("Option<Vec<PostsItems>>"));
}

#[test]
fn rust_array_without_subfields() {
    let col = make_col(
        "posts",
        vec![FieldDefinition::builder("data", FieldType::Array).build()],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("Option<Vec<serde_json::Value>>"));
}

#[test]
fn rust_group_and_blocks_fields() {
    let col = make_col(
        "pages",
        vec![
            FieldDefinition::builder("seo", FieldType::Group).build(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "text",
                    vec![text_field("body", true)],
                )])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(out.contains("Option<serde_json::Value>"), "empty group");
    assert!(out.contains("Option<Vec<serde_json::Value>>"), "blocks");
}

#[test]
fn rust_group_field_with_subfields() {
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    text_field("title", true),
                    text_field("description", false),
                ])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(
        out.contains("pub struct PostsSeo {"),
        "group sub-type: {out}"
    );
    assert!(
        out.contains("pub title: Option<String>,"),
        "group sub-field: {out}"
    );
    assert!(
        out.contains("pub description: Option<String>,"),
        "group optional sub-field: {out}"
    );
}

#[test]
fn rust_array_nested_in_row() {
    let col = make_col(
        "posts",
        vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("items", FieldType::Array)
                        .fields(vec![text_field("label", true)])
                        .build(),
                ])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(
        out.contains("pub struct PostsItems {"),
        "array nested in Row should emit sub-type: {out}"
    );
}

#[test]
fn rust_global_with_subtypes() {
    let mut global = GlobalDefinition::new("settings");
    global.fields = vec![
        FieldDefinition::builder("nav", FieldType::Array)
            .fields(vec![text_field("label", true)])
            .build(),
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![text_field("title", true)])
            .build(),
    ];
    let mut out = String::new();
    render_global(&mut out, &global);
    assert!(
        out.contains("pub struct SettingsNav {"),
        "array sub-type: {out}"
    );
    assert!(
        out.contains("pub struct SettingsSeo {"),
        "group sub-type: {out}"
    );
}

#[test]
fn rust_upload_and_select_fields() {
    let col = make_col(
        "items",
        vec![
            FieldDefinition::builder("image", FieldType::Upload)
                .required(true)
                .build(),
            FieldDefinition::builder("status", FieldType::Select)
                .required(true)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Draft".into()),
                    "draft",
                )])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    // Single upload is optional on read (populate can null it).
    assert!(out.contains("pub image: Option<String>,"), "got: {out}");
    assert!(
        out.contains("pub status: Option<ItemsStatus>,"),
        "select → enum type: {out}"
    );
}

/// Proves the pattern `emit_poly` generates compiles and is sound at every
/// depth: the `"collection/id"` string (depth=0) decodes to `Id`, and the
/// `{collection, ...doc}` object (depth>=1) decodes to the matching variant.
#[test]
fn poly_pattern_compiles_and_roundtrips() {
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct Posts {
        id: String,
        title: String,
    }
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(tag = "collection")]
    enum RelatedRef {
        #[serde(rename = "posts")]
        Posts(Box<Posts>),
    }
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(untagged)]
    enum Related {
        Doc(RelatedRef),
        Id(String),
    }

    // depth=0: the composite id string.
    assert_eq!(
        serde_json::from_str::<Related>("\"posts/a1\"").unwrap(),
        Related::Id("posts/a1".to_string())
    );
    // depth>=1: the populated {collection, ...doc} object.
    let obj = r#"{"collection":"posts","id":"a1","title":"Hello"}"#;
    let got = serde_json::from_str::<Related>(obj).unwrap();
    assert!(matches!(got, Related::Doc(RelatedRef::Posts(_))));
}

/// Proves the pattern `emit_enum` generates actually compiles and behaves:
/// a value not in the option set (e.g. one removed since generation) is
/// preserved in `Other` rather than failing to deserialize, and round-trips.
#[test]
fn lossless_enum_pattern_compiles_and_roundtrips() {
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    #[serde(from = "String", into = "String")]
    enum Status {
        Draft,
        Published,
        Other(String),
    }
    impl From<String> for Status {
        fn from(s: String) -> Self {
            match s.as_str() {
                "draft" => Status::Draft,
                "published" => Status::Published,
                _ => Status::Other(s),
            }
        }
    }
    impl From<Status> for String {
        fn from(v: Status) -> Self {
            match v {
                Status::Draft => "draft".to_string(),
                Status::Published => "published".to_string(),
                Status::Other(s) => s,
            }
        }
    }

    assert_eq!(
        serde_json::from_str::<Status>("\"draft\"").unwrap(),
        Status::Draft
    );
    // A since-removed option value survives instead of erroring.
    assert_eq!(
        serde_json::from_str::<Status>("\"legacy\"").unwrap(),
        Status::Other("legacy".to_string())
    );
    assert_eq!(
        serde_json::to_string(&Status::Other("legacy".to_string())).unwrap(),
        "\"legacy\""
    );
    assert_eq!(serde_json::to_string(&Status::Draft).unwrap(), "\"draft\"");
}

#[test]
fn rust_reserved_self_is_guarded_in_variant_and_type() {
    // A select option value `self` → variant `Self` would be reserved; and a
    // collection named `self` → `struct Self` / `CollectionSlug::Self` too.
    let mut registry = Registry::new();
    registry.register_collection(make_col(
        "self",
        vec![
            FieldDefinition::builder("visibility", FieldType::Select)
                .required(true)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Self".into()),
                    "self",
                )])
                .build(),
        ],
    ));
    let out = render(&registry);

    assert!(
        out.contains("pub struct Self_ {"),
        "self collection → Self_: {out}"
    );
    assert!(
        out.contains("Self_,"),
        "self option value → Self_ variant: {out}"
    );
    assert!(
        out.contains("\"self\" => SelfVisibility::Self_,"),
        "match keeps the raw value: {out}"
    );
    assert!(
        out.contains("#[serde(rename = \"self\")]"),
        "CollectionSlug variant keeps wire slug: {out}"
    );
}

#[test]
fn rust_select_options_emit_lossless_enum() {
    let col = make_col(
        "items",
        vec![
            FieldDefinition::builder("status", FieldType::Select)
                .required(true)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Draft".into()), "draft"),
                    SelectOption::new(LocalizedString::Plain("In Progress".into()), "in-progress"),
                ])
                .build(),
        ],
    );
    let out = {
        let mut r = Registry::new();
        r.register_collection(col);
        render(&r)
    };
    assert!(
        out.contains("#[serde(from = \"String\", into = \"String\")]"),
        "{out}"
    );
    assert!(out.contains("pub enum ItemsStatus {"), "{out}");
    assert!(out.contains("Draft,"), "{out}");
    assert!(
        out.contains("InProgress,"),
        "arbitrary value → valid variant: {out}"
    );
    assert!(
        out.contains("Other(String),"),
        "catch-all preserves unknowns: {out}"
    );
    assert!(
        out.contains("\"in-progress\" => ItemsStatus::InProgress,"),
        "match uses raw value: {out}"
    );
    assert!(
        out.contains("ItemsStatus::Other(s) => s,"),
        "into-String round-trips Other: {out}"
    );
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
        out.contains("pub images: Option<Vec<String>>,"),
        "has-many upload should be Vec<String>: {out}"
    );
}

#[test]
fn rust_global_output() {
    let mut global = GlobalDefinition::new("site_settings");
    global.fields = vec![text_field("site_name", true)];
    let mut out = String::new();
    render_global(&mut out, &global);
    assert!(out.contains("pub struct SiteSettings {"));
    assert!(out.contains("pub id: String,"));
    assert!(out.contains("pub site_name: Option<String>,"));
    assert!(out.contains("pub created_at: Option<String>,"));
}

#[test]
fn rust_full_render() {
    let mut registry = Registry::new();
    registry.register_collection(make_col("posts", vec![text_field("title", true)]));
    let mut settings = GlobalDefinition::new("settings");
    settings.fields = vec![text_field("name", true)];
    registry.register_global(settings);
    let out = render(&registry);
    assert!(out.contains("use serde::{Deserialize, Serialize};"));
    assert!(out.contains("pub struct Posts {"));
    assert!(out.contains("pub struct Settings {"));
}

#[test]
fn rust_collection_slug_enum() {
    let mut registry = Registry::new();
    registry.register_collection(make_col("posts", vec![text_field("title", true)]));
    registry.register_collection(make_col("pages", vec![text_field("body", true)]));
    let out = render(&registry);
    assert!(out.contains("pub enum CollectionSlug {"), "got: {out}");
    assert!(out.contains("#[serde(rename = \"pages\")]"), "got: {out}");
    assert!(out.contains("Pages,"), "got: {out}");
    assert!(out.contains("Posts,"), "got: {out}");
}

#[test]
fn rust_no_collection_slug_when_empty() {
    let out = render(&Registry::new());
    assert!(!out.contains("CollectionSlug"));
}

#[test]
fn rust_no_timestamps() {
    let mut col = make_col("tags", vec![text_field("name", true)]);
    col.timestamps = false;
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(!out.contains("created_at"));
}

#[test]
fn rust_text_has_many() {
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
    assert!(
        out.contains("pub tags: Option<Vec<String>>,"),
        "list: {out}"
    );
    assert!(out.contains("Option<Vec<String>>"), "optional: {out}");
}

#[test]
fn rust_number_has_many() {
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
    assert!(out.contains("pub scores: Option<Vec<f64>>,"), "list: {out}");
    assert!(out.contains("Option<Vec<f64>>"), "optional: {out}");
}

#[test]
fn rust_email_date_richtext_textarea() {
    let col = make_col(
        "items",
        vec![
            FieldDefinition::builder("email", FieldType::Email)
                .required(true)
                .build(),
            FieldDefinition::builder("date", FieldType::Date)
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
    assert!(out.contains("pub email: Option<String>,"));
    assert!(out.contains("pub date: Option<String>,"));
    assert!(out.contains("pub body: Option<String>,"));
    assert!(out.contains("pub notes: Option<String>,"));
}

#[test]
fn rust_code_join_radio_fields() {
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
    assert!(
        out.contains("pub snippet: Option<String>,"),
        "code → String: {out}"
    );
    assert!(
        out.contains("Option<Vec<serde_json::Value>>"),
        "join → Vec<Value>: {out}"
    );
    assert!(
        out.contains("pub color: Option<String>,"),
        "radio no-options → String: {out}"
    );
}

#[test]
fn rust_select_has_many() {
    let col = make_col(
        "items",
        vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .required(true)
                .build(),
            FieldDefinition::builder("sizes", FieldType::Radio)
                .has_many(true)
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(
        out.contains("pub tags: Option<Vec<String>>,"),
        "list: {out}"
    );
    assert!(out.contains("Option<Vec<String>>"), "optional: {out}");
}

#[test]
fn rust_row_collapsible_tabs_promote_subfields() {
    let col = make_col(
        "items",
        vec![
            FieldDefinition::builder("layout_row", FieldType::Row)
                .fields(vec![
                    text_field("first_name", true),
                    text_field("last_name", false),
                ])
                .build(),
            FieldDefinition::builder("details", FieldType::Collapsible)
                .fields(vec![text_field("bio", false)])
                .build(),
            FieldDefinition::builder("sections", FieldType::Tabs)
                .tabs(vec![FieldTab::new(
                    "Tab1",
                    vec![text_field("tab_field", true)],
                )])
                .build(),
        ],
    );
    let mut out = String::new();
    render_collection(&mut out, &col);
    assert!(!out.contains("layout_row"), "row name not a field: {out}");
    assert!(
        out.contains("pub first_name: Option<String>,"),
        "row sub-field: {out}"
    );
    assert!(
        out.contains("pub last_name: Option<String>,"),
        "row optional: {out}"
    );
    assert!(
        !out.contains("details"),
        "collapsible name not a field: {out}"
    );
    assert!(
        out.contains("pub bio: Option<String>,"),
        "collapsible sub: {out}"
    );
    assert!(!out.contains("sections"), "tabs name not a field: {out}");
    assert!(
        out.contains("pub tab_field: Option<String>,"),
        "tabs sub: {out}"
    );
}
