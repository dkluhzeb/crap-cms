//! Golden-snapshot tests: one comprehensive schema rendered by every generator
//! — the four client languages, the Rust proto decoder, and the per-config Lua
//! types — and diffed against a committed file, so any output change fails
//! until reviewed. This is the "can't regress silently" net; the per-generator
//! unit tests pin behavioral intent.
//!
//! The Rust client types and the proto decoder are one contract (`typegen
//! proto` decodes into the `typegen client -l rs` structs), so both are also
//! parsed with `syn` and every decoder is checked to build exactly the fields
//! of the struct it targets. That is structural, not a type check: a true
//! compile of the pair (and a TS/Go/Python compiler check of the others) needs
//! a toolchain at test time, out of scope for the hermetic suite.
//!
//! The Lua types (with the static `types/crap.lua`) are checked against the
//! `LuaLS` annotation grammar hermetically, and by a real `lua-language-server
//! --check` when one is installed (`CRAP_LUALS`, `PATH`, or Mason); without
//! one that test skips. The Lua golden is committed as `kitchen_sink.lua.txt`
//! (like the proto golden's `.proto.txt`) so editors don't analyze the test
//! data as Lua source.
//!
//! Regenerate the goldens after an intentional change with
//! `cargo test -p crap-cms --lib typegen::golden_tests::regenerate -- --ignored`.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
};

use syn::{Expr, File, ImplItem, Item, Member, Stmt, Type, parse_file};

use crate::{
    core::{
        BlockDefinition, CollectionDefinition, FieldAdmin, FieldDefinition, FieldTab, FieldType,
        GlobalDefinition, JoinConfig, LocalizedString, Registry, RelationshipConfig, SelectOption,
        VersionsConfig,
        collection::Auth,
        upload::{CollectionUpload, FormatQuality, ImageSizeBuilder},
    },
    typegen::{
        Language,
        client::generate,
        lua::{self, luals_check},
        rust_proto,
    },
};

/// The command that rewrites every golden from the current generators.
const REGENERATE: &str =
    "cargo test -p crap-cms --lib typegen::golden_tests::regenerate -- --ignored";

/// The module path the proto golden imports the prost types from.
const PROTO_MOD: &str = "crate::proto";

/// Where the golden files live, relative to the crate root.
const GOLDEN_DIR: &str = "src/typegen/client/testdata";

/// Where the Lua types' template-data section starts: the classes after it
/// describe the admin pages, not the schema, and are pinned where they are
/// defined.
const TEMPLATE_DATA_MARKER: &str = "-- ─── Template data context";

fn text(name: &str, required: bool) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Text)
        .required(required)
        .build()
}

fn options(values: &[&str]) -> Vec<SelectOption> {
    values
        .iter()
        .map(|&v| SelectOption::new(LocalizedString::Plain(v.into()), v))
        .collect()
}

fn relationship(name: &str, target: &str, has_many: bool) -> FieldDefinition {
    FieldDefinition::builder(name, FieldType::Relationship)
        .relationship(RelationshipConfig::new(target, has_many))
        .build()
}

fn polymorphic(name: &str, has_many: bool) -> FieldDefinition {
    // Both targets are registered below, so the Rust golden compiles.
    let mut rc = RelationshipConfig::new("users", has_many);
    rc.polymorphic = vec!["users".into(), "tags".into()];

    FieldDefinition::builder(name, FieldType::Relationship)
        .relationship(rc)
        .build()
}

/// A schema exercising the breadth of the generators: every field type, the
/// read-only and write-only keys of both wire shapes, identifier hazards, and
/// the system keys.
fn kitchen_sink() -> Registry {
    let mut reg = Registry::new();

    for col in [posts(), users(), tags(), media(), twofa()] {
        reg.register_collection(col);
    }
    reg.register_global(settings());

    reg
}

/// Drafts, soft delete, timestamps, and every field shape a document holds.
fn posts() -> CollectionDefinition {
    let mut posts = CollectionDefinition::new("posts");
    posts.timestamps = true;
    posts.soft_delete = true;
    posts.versions = Some(VersionsConfig::new(true, 10));
    posts.fields = [
        post_scalars(),
        post_choices(),
        post_references(),
        post_composites(),
        post_layout(),
    ]
    .concat();

    posts
}

/// Scalars: a localized field, a timezone date, a code field with a language
/// companion, JSON and HTML rich text, has-many number and text, a hidden
/// field.
fn post_scalars() -> Vec<FieldDefinition> {
    vec![
        text("title", true),
        FieldDefinition::builder("summary", FieldType::Textarea)
            .localized(true)
            .build(),
        FieldDefinition::builder("published_at", FieldType::Date)
            .timezone(true)
            .build(),
        FieldDefinition::builder("snippet", FieldType::Code)
            .admin(
                FieldAdmin::builder()
                    .languages(vec!["python".to_string(), "rust".to_string()])
                    .build(),
            )
            .build(),
        FieldDefinition::builder("body", FieldType::Richtext)
            .admin(FieldAdmin::builder().richtext_format("json").build())
            .build(),
        FieldDefinition::builder("teaser", FieldType::Richtext).build(),
        FieldDefinition::builder("scores", FieldType::Number)
            .has_many(true)
            .build(),
        FieldDefinition::builder("keywords", FieldType::Text)
            .has_many(true)
            .build(),
        FieldDefinition::builder("active", FieldType::Checkbox).build(),
        FieldDefinition::builder("data", FieldType::Json).build(),
        FieldDefinition::builder("internal_note", FieldType::Text)
            .hidden(true)
            .build(),
    ]
}

/// A required select, a radio, and a has-many select.
fn post_choices() -> Vec<FieldDefinition> {
    vec![
        FieldDefinition::builder("status", FieldType::Select)
            .required(true)
            .options(options(&["draft", "published"]))
            .build(),
        FieldDefinition::builder("layout", FieldType::Radio)
            .options(options(&["grid", "list"]))
            .build(),
        FieldDefinition::builder("categories", FieldType::Select)
            .has_many(true)
            .options(options(&["news", "guides"]))
            .build(),
    ]
}

/// A relationship required on create, has-many, an upload, and polymorphic
/// has-many and has-one.
fn post_references() -> Vec<FieldDefinition> {
    let mut author = relationship("author", "users", false);
    author.required = true;

    let cover = FieldDefinition::builder("cover", FieldType::Upload)
        .relationship(RelationshipConfig::new("media", false))
        .build();

    vec![
        author,
        relationship("tags", "tags", true),
        cover,
        polymorphic("related", true),
        polymorphic("featured", false),
    ]
}

/// A localized group, an array with a nested group, a nested array
/// (array-in-array) and a hidden sub-field, blocks, and an empty group and
/// array.
fn post_composites() -> Vec<FieldDefinition> {
    let hidden_row_field = FieldDefinition::builder("secret", FieldType::Text)
        .hidden(true)
        .build();

    vec![
        FieldDefinition::builder("seo", FieldType::Group)
            .localized(true)
            .fields(vec![text("meta_title", true), text("meta_desc", false)])
            .build(),
        FieldDefinition::builder("items", FieldType::Array)
            .fields(vec![
                text("label", true),
                FieldDefinition::builder("meta", FieldType::Group)
                    .fields(vec![text("key", true)])
                    .build(),
                FieldDefinition::builder("notes", FieldType::Array)
                    .fields(vec![text("body", false)])
                    .build(),
                hidden_row_field,
            ])
            .build(),
        FieldDefinition::builder("content", FieldType::Blocks)
            .blocks(vec![BlockDefinition::new(
                "text",
                vec![
                    FieldDefinition::builder("body", FieldType::Richtext)
                        .required(true)
                        .build(),
                ],
            )])
            .build(),
        FieldDefinition::builder("extra", FieldType::Group).build(),
        FieldDefinition::builder("raw_rows", FieldType::Array).build(),
    ]
}

/// The transparent layout wrappers: a row, a collapsible, and tabs.
fn post_layout() -> Vec<FieldDefinition> {
    vec![
        FieldDefinition::builder("byline_row", FieldType::Row)
            .fields(vec![text("byline", false)])
            .build(),
        FieldDefinition::builder("aside_box", FieldType::Collapsible)
            .fields(vec![text("aside", false)])
            .build(),
        FieldDefinition::builder("sections", FieldType::Tabs)
            .tabs(vec![FieldTab::new("Extra", vec![text("tab_note", true)])])
            .build(),
    ]
}

/// An auth collection (a `password` on input) with a virtual join (read only).
fn users() -> CollectionDefinition {
    let mut users = CollectionDefinition::new("users");
    users.timestamps = true;
    users.auth = Some(Auth::new(true));
    // `email` first, required and unique — the field the parser injects into
    // every auth collection.
    users.fields = vec![
        FieldDefinition::builder("email", FieldType::Email)
            .required(true)
            .unique(true)
            .build(),
        text("name", true),
        FieldDefinition::builder("authored", FieldType::Join)
            .join(JoinConfig::new("posts", "author"))
            .build(),
    ];

    users
}

fn tags() -> CollectionDefinition {
    let mut tags = CollectionDefinition::new("tags");
    tags.fields = vec![text("name", true)];

    tags
}

/// An upload collection as the schema parser leaves it: the injected upload
/// columns — the per-size ones of one image size with a `webp` variant
/// included — before the user's `alt`.
fn media() -> CollectionDefinition {
    let mut upload = CollectionUpload::new();
    upload.image_sizes = vec![
        ImageSizeBuilder::new("thumbnail")
            .width(200)
            .height(200)
            .build(),
    ];
    upload.format_options.webp = Some(FormatQuality::new(80, false));

    let mut media = CollectionDefinition::new("media");
    media.fields = upload_columns(&upload);
    media.fields.push(text("alt", false));
    media.upload = Some(upload);

    media
}

/// The columns the schema parser injects into an upload collection, in its
/// order.
fn upload_columns(upload: &CollectionUpload) -> Vec<FieldDefinition> {
    let number = |name: &str| FieldDefinition::builder(name, FieldType::Number).build();

    let mut columns = vec![
        text("filename", true),
        text("mime_type", false),
        number("filesize"),
        number("width"),
        number("height"),
        text("url", false),
        number("focal_x"),
        number("focal_y"),
    ];
    columns.extend(
        upload
            .size_columns()
            .into_iter()
            .map(|(name, ty)| FieldDefinition::builder(name, ty).build()),
    );

    columns
}

/// Identifier hazards: a leading-digit slug and field, a keyword field in the
/// client languages (`type`), a Lua reserved word (`end`) and a `LuaLS`
/// field-scope word (`private`).
fn twofa() -> CollectionDefinition {
    let mut twofa = CollectionDefinition::new("2fa");
    twofa.fields = vec![
        text("type", true),
        text("2fa", false),
        text("end", false),
        text("private", false),
    ];

    twofa
}

/// A global with drafts and localized fields.
fn settings() -> GlobalDefinition {
    let mut settings = GlobalDefinition::new("settings");
    settings.versions = Some(VersionsConfig::new(true, 5));
    settings.fields = vec![
        FieldDefinition::builder("site_name", FieldType::Text)
            .required(true)
            .localized(true)
            .build(),
        FieldDefinition::builder("nav", FieldType::Array)
            .fields(vec![text("label", true), text("url", true)])
            .build(),
    ];

    settings
}

/// Every golden file with the source it pins.
fn goldens(reg: &Registry) -> Vec<(&'static str, String)> {
    let client = |lang| generate(reg, lang).expect("generate golden");

    vec![
        ("kitchen_sink.rs", client(Language::Rust)),
        ("kitchen_sink.ts", client(Language::Typescript)),
        ("kitchen_sink.go", client(Language::Go)),
        ("kitchen_sink.py", client(Language::Python)),
        ("kitchen_sink.proto.txt", rust_proto::render(reg, PROTO_MOD)),
        ("kitchen_sink.lua.txt", lua_schema_types(reg)),
    ]
}

/// The per-schema part of the Lua types (everything before the template-data
/// section).
fn lua_schema_types(reg: &Registry) -> String {
    let out = lua::render(reg).expect("render the Lua types");
    let end = out
        .find(TEMPLATE_DATA_MARKER)
        .expect("the Lua types render the template-data section");

    out[..end].to_string()
}

/// Regenerate every golden. Ignored by default (it writes into the source
/// tree); run explicitly after an intentional generator change.
#[test]
#[ignore = "writes golden files into client/testdata/; run with --ignored to regenerate"]
fn regenerate() {
    let dir = format!("{}/{GOLDEN_DIR}", env!("CARGO_MANIFEST_DIR"));
    fs::create_dir_all(&dir).expect("create testdata dir");

    for (file, source) in goldens(&kitchen_sink()) {
        fs::write(format!("{dir}/{file}"), source).expect("write golden");
    }
}

/// The committed golden files, in the order [`goldens`] renders them.
const COMMITTED: [(&str, &str); 6] = [
    (
        "kitchen_sink.rs",
        include_str!("client/testdata/kitchen_sink.rs"),
    ),
    (
        "kitchen_sink.ts",
        include_str!("client/testdata/kitchen_sink.ts"),
    ),
    (
        "kitchen_sink.go",
        include_str!("client/testdata/kitchen_sink.go"),
    ),
    (
        "kitchen_sink.py",
        include_str!("client/testdata/kitchen_sink.py"),
    ),
    (
        "kitchen_sink.proto.txt",
        include_str!("client/testdata/kitchen_sink.proto.txt"),
    ),
    (
        "kitchen_sink.lua.txt",
        include_str!("client/testdata/kitchen_sink.lua.txt"),
    ),
];

fn assert_golden(file: &str) {
    let Some((_, actual)) = goldens(&kitchen_sink())
        .into_iter()
        .find(|(name, _)| *name == file)
    else {
        panic!("no golden named {file}");
    };

    let Some(&(_, expected)) = COMMITTED.iter().find(|(name, _)| *name == file) else {
        panic!("{file} is not committed");
    };

    assert_eq!(
        actual, expected,
        "{file} golden is stale — regenerate with `{REGENERATE}`"
    );
}

#[test]
fn golden_rust() {
    assert_golden("kitchen_sink.rs");
}

#[test]
fn golden_typescript() {
    assert_golden("kitchen_sink.ts");
}

#[test]
fn golden_go() {
    assert_golden("kitchen_sink.go");
}

#[test]
fn golden_python() {
    assert_golden("kitchen_sink.py");
}

#[test]
fn golden_proto() {
    assert_golden("kitchen_sink.proto.txt");
}

#[test]
fn golden_lua() {
    assert_golden("kitchen_sink.lua.txt");
}

/// The files an editor's `LuaLS` loads for the kitchen-sink project: the
/// static API surface and the per-schema types, in full (template-data
/// section included).
fn kitchen_sink_lua_files() -> [(&'static str, String); 2] {
    let hooks = lua::render(&kitchen_sink()).expect("render the Lua types");

    [
        ("crap.lua", lua::render_static_file()),
        ("hooks.lua", hooks),
    ]
}

/// Every class/alias name, field key, statement and nested function type the
/// Lua generators emit parses as intended under the `LuaLS` annotation
/// grammar — the leading-digit, reserved-word and scope-word names of the
/// kitchen sink included.
#[test]
fn lua_types_follow_the_luals_grammar() {
    for (file, source) in kitchen_sink_lua_files() {
        let violations = luals_check::grammar_violations(&source);

        assert!(violations.is_empty(), "{file}:\n{}", violations.join("\n"));
    }
}

/// A real `LuaLS --check` of the kitchen-sink types reports nothing at
/// Warning level. Skipped (with a note) when no `lua-language-server` is
/// found — set `CRAP_LUALS` to its path, or put it on `PATH`.
#[test]
fn lua_types_pass_a_luals_check() {
    let files = kitchen_sink_lua_files();
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(name, source)| (*name, source.as_str()))
        .collect();

    let Some(result) = luals_check::luals_diagnostics(&files) else {
        eprintln!(
            "skipping the LuaLS check: no lua-language-server found (set {} or add it to PATH)",
            luals_check::LUALS_ENV
        );
        return;
    };

    if let Err(report) = result {
        panic!("LuaLS reports diagnostics for the generated Lua types:\n{report}");
    }
}

/// Each struct the Rust client declares, with its field names.
fn client_structs(file: &File) -> HashMap<String, BTreeSet<String>> {
    file.items
        .iter()
        .filter_map(|item| {
            let Item::Struct(def) = item else {
                return None;
            };

            let fields = def
                .fields
                .iter()
                .filter_map(|f| f.ident.as_ref().map(ToString::to_string))
                .collect();

            Some((def.ident.to_string(), fields))
        })
        .collect()
}

/// A struct literal's field name.
fn member_name(member: &Member) -> String {
    match member {
        Member::Named(ident) => ident.to_string(),
        Member::Unnamed(index) => index.index.to_string(),
    }
}

/// The fields each inherent decoder (`from_document` / `from_struct`) sets in
/// the `Self { … }` it returns, by the type it builds.
fn proto_constructors(file: &File) -> Vec<(String, BTreeSet<String>)> {
    let mut out = Vec::new();

    for item in &file.items {
        let Item::Impl(imp) = item else { continue };
        let Type::Path(ty) = imp.self_ty.as_ref() else {
            continue;
        };
        if imp.trait_.is_some() {
            continue;
        }

        let name = ty
            .path
            .segments
            .last()
            .expect("type name")
            .ident
            .to_string();

        for member in &imp.items {
            let ImplItem::Fn(func) = member else { continue };
            let Some(Stmt::Expr(Expr::Struct(ctor), None)) = func.block.stmts.last() else {
                continue;
            };

            let fields = ctor.fields.iter().map(|f| member_name(&f.member)).collect();
            out.push((name.clone(), fields));
        }
    }

    out
}

/// `typegen proto` decodes into the `typegen client -l rs` structs, so the two
/// must agree on every struct and field: a decoder setting a field the struct
/// lacks (or missing one it has) fails to compile in the user's crate. Every
/// single-locale struct has a decoder; the `locale = "all"` shape has none.
#[test]
fn proto_decoders_build_exactly_the_client_structs() {
    let reg = kitchen_sink();
    let client = parse_file(&generate(&reg, Language::Rust).expect("generate"))
        .expect("the Rust client types parse");
    let proto = parse_file(&rust_proto::render(&reg, PROTO_MOD)).expect("the proto decoder parses");

    let structs = client_structs(&client);
    let ctors = proto_constructors(&proto);

    for (name, fields) in &ctors {
        let declared = structs
            .get(name)
            .unwrap_or_else(|| panic!("proto builds `{name}`, which the client does not declare"));
        assert_eq!(fields, declared, "`{name}`: proto sets vs client declares");
    }

    let built: HashSet<&str> = ctors.iter().map(|(name, _)| name.as_str()).collect();
    for name in structs.keys() {
        if name.ends_with("Localized") {
            continue;
        }
        assert!(
            built.contains(name.as_str()),
            "client struct `{name}` has no proto decoder"
        );
    }
}
