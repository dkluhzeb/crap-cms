//! Cross-language checks of the two wire shapes the generated client types
//! describe (see `core::upload::read_shape`).
//!
//! - **Read.** An upload collection's per-size columns never reach a client
//!   (the read folds them into one `sizes` object), a JSON-format rich text
//!   field is a JSON document rather than a string, a `hidden` field is
//!   stripped from every read, and a populated document carries its
//!   `collection` tag. Every language is checked, because a
//!   declared-but-never-sent key is a hard decode failure in the strict ones.
//! - **Write.** The input (`…Data`) types accept what a create or update
//!   accepts: no server-derived upload column, no virtual join, references as
//!   ids, and an auth collection's `password`.

use crate::{
    core::{
        CollectionDefinition, FieldAdmin, FieldDefinition, FieldType, GlobalDefinition, Registry,
        RelationshipConfig, VersionsConfig,
        collection::Auth,
        upload::{CollectionUpload, FormatQuality, ImageSizeBuilder},
    },
    typegen::Language,
};

use super::generate;

/// A `media` collection with one image size and a WebP variant — the
/// columns `inject_upload_fields` would add, and the upload config that
/// makes the read fold them away.
fn media_with_sizes() -> Registry {
    let mut upload = CollectionUpload::new();
    upload.image_sizes = vec![
        ImageSizeBuilder::new("thumbnail")
            .width(200)
            .height(200)
            .build(),
    ];
    upload.format_options.webp = Some(FormatQuality::new(80, false));

    let mut media = CollectionDefinition::new("media");
    media.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text)
            .required(true)
            .build(),
    ];
    media.fields.extend(
        upload
            .size_columns()
            .into_iter()
            .map(|(name, ty)| FieldDefinition::builder(name, ty).build()),
    );
    media.upload = Some(upload);

    let mut reg = Registry::new();
    reg.register_collection(media);
    reg
}

/// The per-size columns `assemble_sizes_object` removes from every read.
const STRIPPED: [&str; 4] = [
    "thumbnail_url",
    "thumbnail_width",
    "thumbnail_height",
    "thumbnail_webp_url",
];

/// The first generated line mentioning a wire key — every language keeps
/// the raw key somewhere on the declaring line (a property name, a JSON
/// tag, a serde rename), so this reads the declaration without depending
/// on a printer's column alignment.
fn decl_line<'a>(out: &'a str, key: &str) -> &'a str {
    out.lines()
        .find(|line| line.contains(key))
        .unwrap_or_else(|| panic!("nothing declares `{key}` in:\n{out}"))
}

fn assert_folds_sizes(lang: Language) {
    let out = generate(&media_with_sizes(), lang).expect("generate");

    for column in STRIPPED {
        assert!(
            !out.contains(column),
            "{lang:?} declares `{column}`, which no read ever carries:\n{out}"
        );
    }

    let sizes = decl_line(&out, "sizes");
    assert!(
        sizes.contains("MediaSizes"),
        "{lang:?} must declare the assembled object, got `{sizes}`"
    );
    assert!(
        out.contains("MediaSizesThumbnailFormatsWebp"),
        "{lang:?} must describe the per-format nesting:\n{out}"
    );
}

#[test]
fn typescript_folds_upload_sizes() {
    assert_folds_sizes(Language::Typescript);
}

#[test]
fn go_folds_upload_sizes() {
    assert_folds_sizes(Language::Go);
}

#[test]
fn python_folds_upload_sizes() {
    assert_folds_sizes(Language::Python);
}

#[test]
fn rust_folds_upload_sizes() {
    assert_folds_sizes(Language::Rust);
}

/// A rich text field with `admin.format = "json"` and a default
/// (HTML) one beside it.
fn richtext_registry() -> Registry {
    let mut pages = CollectionDefinition::new("pages");
    pages.fields = vec![
        FieldDefinition::builder("body", FieldType::Richtext)
            .admin(FieldAdmin::builder().richtext_format("json").build())
            .build(),
        FieldDefinition::builder("teaser", FieldType::Richtext).build(),
    ];

    let mut reg = Registry::new();
    reg.register_collection(pages);
    reg
}

fn assert_json_richtext(lang: Language, json_ty: &str, html_ty: &str) {
    let out = generate(&richtext_registry(), lang).expect("generate");

    let body = decl_line(&out, "body");
    assert!(
        body.contains(json_ty),
        "{lang:?} must type JSON rich text as a JSON document, got `{body}`"
    );

    let teaser = decl_line(&out, "teaser");
    assert!(
        teaser.contains(html_ty),
        "{lang:?} must keep HTML rich text a string, got `{teaser}`"
    );
}

#[test]
fn typescript_types_json_richtext_as_a_document() {
    assert_json_richtext(Language::Typescript, "unknown", "string");
}

#[test]
fn go_types_json_richtext_as_a_document() {
    assert_json_richtext(Language::Go, "interface{}", "*string");
}

#[test]
fn python_types_json_richtext_as_a_document() {
    assert_json_richtext(Language::Python, "Optional[Any]", "Optional[str]");
}

#[test]
fn rust_types_json_richtext_as_a_document() {
    assert_json_richtext(
        Language::Rust,
        "Option<serde_json::Value>",
        "Option<String>",
    );
}

/// One generated TypeScript `export interface` block, from its header to its
/// closing brace.
fn ts_block<'a>(out: &'a str, header: &str) -> &'a str {
    let start = out
        .find(header)
        .unwrap_or_else(|| panic!("{header} not emitted:\n{out}"));
    let rest = &out[start..];

    &rest[..rest.find("\n}").map_or(rest.len(), |i| i + 2)]
}

/// A `media` upload collection as the schema parser leaves it: the injected
/// upload columns (the per-size ones included) before the user's `alt`.
fn injected_media() -> CollectionDefinition {
    let mut upload = CollectionUpload::new();
    upload.image_sizes = vec![
        ImageSizeBuilder::new("thumbnail")
            .width(200)
            .height(200)
            .build(),
    ];

    let mut media = CollectionDefinition::new("media");
    media.fields = vec![
        FieldDefinition::builder("filename", FieldType::Text)
            .required(true)
            .build(),
        FieldDefinition::builder("mime_type", FieldType::Text).build(),
        FieldDefinition::builder("url", FieldType::Text).build(),
        FieldDefinition::builder("focal_x", FieldType::Number).build(),
    ];
    media.fields.extend(
        upload
            .size_columns()
            .into_iter()
            .map(|(name, ty)| FieldDefinition::builder(name, ty).build()),
    );
    media
        .fields
        .push(FieldDefinition::builder("alt", FieldType::Text).build());
    media.upload = Some(upload);

    media
}

/// Regression: the create input of an upload collection declared the
/// server-derived columns — `filename` even required — though every
/// untrusted write has them stripped, so a typed client was forced to send
/// values the server discards.
#[test]
fn typescript_input_omits_server_derived_upload_columns() {
    let mut reg = Registry::new();
    reg.register_collection(injected_media());

    let out = generate(&reg, Language::Typescript).expect("generate");
    let data = ts_block(&out, "export interface MediaData {");

    for derived in ["filename", "mime_type", "url", "thumbnail", "sizes"] {
        assert!(!data.contains(derived), "{derived}: {data}");
    }
    assert!(data.contains("  focal_x?: number | null;"), "{data}");
    assert!(data.contains("  alt?: string | null;"), "{data}");

    let doc = ts_block(&out, "export interface MediaDocument {");
    assert!(doc.contains("  url?: string | null;"), "{doc}");
    assert!(doc.contains("  sizes?: MediaSizes | null;"), "{doc}");
}

/// Regression: an auth collection's create input had no `password`, which
/// gRPC and MCP accept (and the service hashes) beside the fields. No read
/// type declares it.
#[test]
fn auth_input_takes_a_password_no_read_declares_it() {
    let mut users = CollectionDefinition::new("users");
    users.auth = Some(Auth::new(true));
    users.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .build(),
    ];
    let mut reg = Registry::new();
    reg.register_collection(users);

    let ts = generate(&reg, Language::Typescript).expect("generate");
    let data = ts_block(&ts, "export interface UsersData {");
    assert!(data.contains("  password?: string;"), "{data}");
    let doc = ts_block(&ts, "export interface UsersDocument {");
    assert!(!doc.contains("password"), "{doc}");

    for lang in [Language::Go, Language::Python, Language::Rust] {
        let out = generate(&reg, lang).expect("generate");
        assert!(!out.contains("password"), "{lang:?}:\n{out}");
    }
}

/// Regression: a `hidden = true` field is stripped from every read, yet every
/// read type declared it — at the top level and inside a group. It stays in
/// the input: a write may still set it.
#[test]
fn hidden_fields_are_input_only() {
    let secret = || {
        FieldDefinition::builder("secret", FieldType::Text)
            .hidden(true)
            .build()
    };
    let mut posts = CollectionDefinition::new("posts");
    posts.fields = vec![
        FieldDefinition::builder("title", FieldType::Text).build(),
        secret(),
        FieldDefinition::builder("seo", FieldType::Group)
            .fields(vec![
                FieldDefinition::builder("meta", FieldType::Text).build(),
                secret(),
            ])
            .build(),
    ];
    let mut reg = Registry::new();
    reg.register_collection(posts);

    for lang in [Language::Go, Language::Python, Language::Rust] {
        let out = generate(&reg, lang).expect("generate");
        assert!(!out.contains("secret"), "{lang:?}:\n{out}");
    }

    let ts = generate(&reg, Language::Typescript).expect("generate");
    assert!(!ts_block(&ts, "export interface PostsDocument {").contains("secret"));
    assert!(!ts_block(&ts, "export interface PostsSeo {").contains("secret"));
    assert!(ts_block(&ts, "export interface PostsData {").contains("  secret?: string | null;"));
    assert!(ts_block(&ts, "export interface PostsSeoData {").contains("  secret?: string | null;"));
}

/// Regression: a document populated into a relationship carries its
/// `collection` key, but no read type declared it — so a polymorphic union
/// of documents could not be narrowed. A global is never populated.
/// (`collection` is a reserved field name, so no field can claim the key.)
#[test]
fn populated_documents_declare_their_collection_tag() {
    let mut tags = CollectionDefinition::new("tags");
    tags.fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];
    let mut reg = Registry::new();
    reg.register_collection(tags);
    reg.register_global(GlobalDefinition::new("settings"));

    let ts = generate(&reg, Language::Typescript).expect("generate");
    let doc = ts_block(&ts, "export interface TagsDocument {");
    assert!(doc.contains("  collection?: \"tags\";"), "{doc}");
    assert!(!ts_block(&ts, "export interface TagsData {").contains("collection"));
    assert!(!ts_block(&ts, "export interface SettingsDocument {").contains("collection"));

    let py = generate(&reg, Language::Python).expect("generate");
    assert!(
        py.contains("collection: Optional[Literal[\"tags\"]] = None"),
        "{py}"
    );

    let go = generate(&reg, Language::Go).expect("generate");
    assert!(
        go.contains("Collection *string `json:\"collection,omitempty\"`"),
        "{go}"
    );

    // Rust's polymorphic enums consume the key as their serde tag.
    let rs = generate(&reg, Language::Rust).expect("generate");
    let start = rs.find("pub struct Tags {").expect("Tags struct");
    let tags = &rs[start..];
    let tags = &tags[..tags.find("\n}").expect("struct end")];
    assert!(!tags.contains("collection"), "{tags}");
}

/// A global read populates its relationships to `depth` like a collection
/// read, so a global's reference is typed as its id or the populated document.
#[test]
fn a_global_reference_is_typed_as_id_or_document() {
    let mut tags = CollectionDefinition::new("tags");
    tags.fields = vec![FieldDefinition::builder("name", FieldType::Text).build()];
    let mut settings = GlobalDefinition::new("settings");
    settings.fields = vec![
        FieldDefinition::builder("featured", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", false))
            .build(),
        FieldDefinition::builder("pinned", FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", true))
            .build(),
    ];
    let mut reg = Registry::new();
    reg.register_collection(tags);
    reg.register_global(settings);

    let ts = generate(&reg, Language::Typescript).expect("generate");
    let doc = ts_block(&ts, "export interface SettingsDocument {");
    assert!(
        doc.contains("  featured?: string | TagsDocument | null;"),
        "{doc}"
    );
    assert!(
        doc.contains("  pinned?: (string | TagsDocument)[] | null;"),
        "{doc}"
    );
}

/// Regression: `_status` was typed as any string in every client language
/// while the Lua types narrowed it to its two values.
#[test]
fn draft_status_is_its_value_set() {
    let mut posts = CollectionDefinition::new("posts");
    posts.versions = Some(VersionsConfig::new(true, 10));
    let mut reg = Registry::new();
    reg.register_collection(posts);

    let ts = generate(&reg, Language::Typescript).expect("generate");
    assert!(
        ts.contains("  _status?: \"draft\" | \"published\" | null;"),
        "{ts}"
    );

    let py = generate(&reg, Language::Python).expect("generate");
    assert!(
        py.contains("_status: Optional[Literal[\"draft\", \"published\"]] = None"),
        "{py}"
    );

    // Go and Rust keep a plain string (no string-literal types).
    let go = generate(&reg, Language::Go).expect("generate");
    assert!(go.contains("DraftStatus *string"), "{go}");
    let rs = generate(&reg, Language::Rust).expect("generate");
    assert!(rs.contains("pub _status: Option<String>,"), "{rs}");
}

/// The input carries every reference as its id: a polymorphic one as its
/// `"collection/id"` string, never a populated document.
#[test]
fn typescript_input_references_are_ids() {
    let mut rc = RelationshipConfig::new("users", true);
    rc.polymorphic = vec!["users".into(), "tags".into()];
    let mut posts = CollectionDefinition::new("posts");
    posts.fields = vec![
        FieldDefinition::builder("related", FieldType::Relationship)
            .relationship(rc)
            .build(),
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                FieldDefinition::builder("owner", FieldType::Relationship)
                    .required(true)
                    .relationship(RelationshipConfig::new("users", false))
                    .build(),
            ])
            .build(),
    ];
    let mut reg = Registry::new();
    reg.register_collection(posts);
    reg.register_collection(CollectionDefinition::new("users"));
    reg.register_collection(CollectionDefinition::new("tags"));

    let ts = generate(&reg, Language::Typescript).expect("generate");

    let data = ts_block(&ts, "export interface PostsData {");
    assert!(data.contains("  related?: string[] | null;"), "{data}");
    let row = ts_block(&ts, "export interface PostsItemsData {");
    assert!(row.contains("  owner: string;"), "{row}");

    let read_row = ts_block(&ts, "export interface PostsItems {");
    assert!(
        read_row.contains("  owner?: string | UsersDocument | null;"),
        "{read_row}"
    );
}
