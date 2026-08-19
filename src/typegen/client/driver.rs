//! The shared schema walk: resolve every field to the language-neutral IR
//! ([`super::ir`]) and stream `SubType`/`Document` constructs to a
//! [`ClientPrinter`]. Written once here instead of four times across the
//! per-language backends.

use std::{collections::HashSet, slice::from_ref};

use crate::{
    core::{
        CollectionDefinition, FieldDefinition, FieldType, Registry, collection::GlobalDefinition,
        flatten_array_sub_fields,
    },
    typegen::{
        Language,
        helpers::{
            collect_sub_type_fields, is_optional, is_single_ref, rel_has_many,
            sorted_collection_slugs, sorted_global_slugs, to_pascal_case,
        },
    },
};

use super::{
    go,
    ir::{ClientPrinter, Document, EnumDef, Field, FieldTy, PolyDef, SubType},
    python, rust, typescript,
};

/// Render all client types for `lang` from the registry.
///
/// # Errors
///
/// Fails if two schema constructs would generate the same top-level type name
/// (e.g. a collection `posts_status` and a `posts.status` select both map to
/// `PostsStatus`) — emitting that silently would shadow one type or, worse,
/// bind a field to the wrong enum's values.
pub(in crate::typegen) fn generate(registry: &Registry, lang: Language) -> anyhow::Result<String> {
    check_type_name_collisions(registry)?;

    let printer: Box<dyn ClientPrinter> = match lang {
        Language::Typescript => Box::new(typescript::TsPrinter::new()),
        Language::Go => Box::new(go::GoPrinter::new()),
        Language::Python => Box::new(python::PyPrinter::new()),
        Language::Rust => Box::new(rust::RustPrinter::new()),
    };
    Ok(drive(registry, printer))
}

/// The shared schema walk: prelude → each collection's sub-types + document →
/// each global's sub-types + document → epilogue. Public within the module so
/// the per-language tests can drive a specific printer directly (production goes
/// through [`generate`], which selects the printer by language).
pub(in crate::typegen) fn drive(
    registry: &Registry,
    mut printer: Box<dyn ClientPrinter>,
) -> String {
    printer.prelude();

    // Named auxiliary types (select enums + polymorphic-relationship enums) are
    // declared once after the structs; collect them (deduped by name) as each
    // struct's fields are resolved.
    let mut aux = Aux::default();

    for slug in sorted_collection_slugs(registry) {
        let col = &registry.collections[slug];
        emit_owner(
            printer.as_mut(),
            &col.fields,
            &to_pascal_case(&col.slug),
            &mut aux,
        );
        let doc = collection_document(col);
        collect_aux(&doc.fields, &mut aux);
        printer.document(&doc);
    }

    for slug in sorted_global_slugs(registry) {
        let global = &registry.globals[slug];
        emit_owner(
            printer.as_mut(),
            &global.fields,
            &to_pascal_case(&global.slug),
            &mut aux,
        );
        let doc = global_document(global);
        collect_aux(&doc.fields, &mut aux);
        printer.document(&doc);
    }

    printer.enum_types(&aux.enums);
    printer.poly_types(&aux.polys);
    printer.epilogue(registry);
    printer.finish()
}

/// The named types collected during the walk (deduped by their unique names).
#[derive(Default)]
struct Aux {
    enums: Vec<EnumDef>,
    polys: Vec<PolyDef>,
    seen: HashSet<String>,
}

/// Emit every sub-type an owner's fields declare, in declaration order,
/// collecting each sub-type's auxiliary defs along the way.
fn emit_owner(
    printer: &mut dyn ClientPrinter,
    fields: &[FieldDefinition],
    root_pascal: &str,
    aux: &mut Aux,
) {
    for stf in collect_sub_type_fields(fields, root_pascal) {
        let name = format!("{}{}", stf.parent_pascal, to_pascal_case(&stf.field.name));
        let sub_pascal = name.clone();
        let sub = SubType {
            name,
            kind: stf.kind,
            field_name: &stf.field.name,
            fields: resolve_fields(&stf.field.fields, &sub_pascal),
        };
        collect_aux(&sub.fields, aux);
        printer.sub_type(&sub);
    }
}

/// Record each field's named auxiliary type (select enum or polymorphic enum),
/// deduped by name — the names are globally unique (owner + field `PascalCase`).
fn collect_aux(fields: &[Field], aux: &mut Aux) {
    for f in fields {
        match &f.ty {
            FieldTy::Enum { name, values, .. } if aux.seen.insert(name.clone()) => {
                aux.enums.push(EnumDef {
                    name: name.clone(),
                    values: values.clone(),
                });
            }
            FieldTy::PolyRel { name, targets, .. } if aux.seen.insert(name.clone()) => {
                aux.polys.push(PolyDef {
                    name: name.clone(),
                    targets: targets.clone(),
                });
            }
            _ => {}
        }
    }
}

/// Build the [`Document`] for a collection.
fn collection_document(col: &CollectionDefinition) -> Document<'_> {
    let root = to_pascal_case(&col.slug);
    Document {
        fields: resolve_fields(&col.fields, &root),
        name: root,
        slug: &col.slug,
        timestamps: col.timestamps,
        is_global: false,
        select_options: select_field_options(&col.fields),
    }
}

/// Build the [`Document`] for a global (always timestamped, no select docstring).
fn global_document(global: &GlobalDefinition) -> Document<'_> {
    let root = to_pascal_case(&global.slug);
    Document {
        fields: resolve_fields(&global.fields, &root),
        name: root,
        slug: &global.slug,
        timestamps: true,
        is_global: true,
        select_options: Vec::new(),
    }
}

/// Resolve a field list to the IR, flattening transparent layout wrappers
/// (Row/Collapsible/Tabs) so a printer never sees them. `parent_pascal` is the
/// enclosing owner's compound `PascalCase`, used to name nested sub-types.
fn resolve_fields<'a>(fields: &'a [FieldDefinition], parent_pascal: &str) -> Vec<Field<'a>> {
    let mut out = Vec::new();
    for field in fields {
        push_resolved(&mut out, field, parent_pascal);
    }
    out
}

/// Resolve one field into `out`, recursing through a transparent layout wrapper.
fn push_resolved<'a>(out: &mut Vec<Field<'a>>, field: &'a FieldDefinition, parent_pascal: &str) {
    if field.field_type.is_layout_wrapper() {
        for sub in flatten_array_sub_fields(from_ref(field)) {
            push_resolved(out, sub, parent_pascal);
        }
        return;
    }
    out.push(Field {
        name: &field.name,
        ty: resolve_ty(field, parent_pascal),
        // A single relationship/upload is optional on read even when `required`:
        // population nulls it when the target is soft-deleted or access-denied
        // (has-many drops the entry instead), so a non-optional type would lie.
        optional: is_optional(field) || is_single_ref(field),
    });
}

/// Map a field to its language-neutral [`FieldTy`]. Assumes a complete registry:
/// a relationship/upload whose target collection is missing is a schema error
/// that registry-level validation should reject before generation (see the
/// module note), not something to silently paper over here.
///
/// Public within `typegen` so the Rust proto-conversion generator (`rust_proto`)
/// drives its decode dispatch off the *same* resolved type as the client type
/// generator — the two can't disagree about what a field's type is.
pub(in crate::typegen) fn resolve_ty(field: &FieldDefinition, parent_pascal: &str) -> FieldTy {
    match &field.field_type {
        FieldType::Text if field.has_many => FieldTy::StrList,
        FieldType::Text
        | FieldType::Textarea
        | FieldType::Email
        | FieldType::Date
        | FieldType::Richtext
        | FieldType::Code => FieldTy::Str,
        FieldType::Number if field.has_many => FieldTy::NumList,
        FieldType::Number => FieldTy::Num,
        FieldType::Checkbox => FieldTy::Bool,
        FieldType::Json => FieldTy::Json,
        FieldType::Select | FieldType::Radio => {
            if field.options.is_empty() {
                if field.has_many {
                    FieldTy::StrList
                } else {
                    FieldTy::Str
                }
            } else {
                FieldTy::Enum {
                    name: format!("{parent_pascal}{}", to_pascal_case(&field.name)),
                    values: field.options.iter().map(|o| o.value.clone()).collect(),
                    many: field.has_many,
                }
            }
        }
        FieldType::Upload => {
            let target = field
                .relationship
                .as_ref()
                .filter(|rc| !rc.collection.is_empty())
                .map(|rc| to_pascal_case(&rc.collection));
            FieldTy::Rel {
                target,
                many: rel_has_many(field),
            }
        }
        FieldType::Relationship => match &field.relationship {
            Some(rc) if rc.is_polymorphic() => FieldTy::PolyRel {
                name: format!("{parent_pascal}{}", to_pascal_case(&field.name)),
                targets: rc.all_collections().into_iter().map(Into::into).collect(),
                many: rc.has_many,
            },
            Some(rc) => FieldTy::Rel {
                target: Some(to_pascal_case(&rc.collection)),
                many: rc.has_many,
            },
            None => FieldTy::Rel {
                target: None,
                many: false,
            },
        },
        FieldType::Array if field.fields.is_empty() => FieldTy::JsonList,
        FieldType::Array => FieldTy::SubType {
            name: format!("{}{}", parent_pascal, to_pascal_case(&field.name)),
            list: true,
        },
        FieldType::Group if field.fields.is_empty() => FieldTy::Map,
        FieldType::Group => FieldTy::SubType {
            name: format!("{}{}", parent_pascal, to_pascal_case(&field.name)),
            list: false,
        },
        FieldType::Blocks | FieldType::Join => FieldTy::JsonList,
        // Layout wrappers are flattened in `push_resolved` before this is called.
        FieldType::Row | FieldType::Collapsible | FieldType::Tabs => {
            unreachable!("layout wrappers are flattened before type resolution")
        }
    }
}

/// Top-level `Select` fields with non-empty options, as `(raw_name, raw_values)`.
fn select_field_options(fields: &[FieldDefinition]) -> Vec<(String, Vec<String>)> {
    fields
        .iter()
        .filter(|f| f.field_type == FieldType::Select && !f.options.is_empty())
        .map(|f| {
            let values = f.options.iter().map(|o| o.value.clone()).collect();
            (f.name.clone(), values)
        })
        .collect()
}

/// Fail if two schema constructs would emit the same top-level type name — a
/// silent collision would shadow one type, or bind a field to the wrong enum's
/// values (auxiliary types are deduped by name during the walk).
fn check_type_name_collisions(registry: &Registry) -> anyhow::Result<()> {
    let mut seen = HashSet::new();
    for name in all_type_names(registry) {
        anyhow::ensure!(
            seen.insert(name.clone()),
            "generated type name `{name}` is produced by two different schema constructs \
             (a collection/global, a sub-type, a select enum, or a polymorphic-relationship \
             enum). Rename one — e.g. a collection `posts_status` and a `posts.status` select \
             both map to `PostsStatus`."
        );
    }
    Ok(())
}

/// Every top-level type name the generator would emit, in walk order (with
/// duplicates preserved so [`check_type_name_collisions`] can spot them).
fn all_type_names(registry: &Registry) -> Vec<String> {
    let mut names = Vec::new();

    for slug in sorted_collection_slugs(registry) {
        let col = &registry.collections[slug];
        collect_type_names(&col.fields, &to_pascal_case(&col.slug), &mut names);
    }
    for slug in sorted_global_slugs(registry) {
        let global = &registry.globals[slug];
        collect_type_names(&global.fields, &to_pascal_case(&global.slug), &mut names);
    }
    if !registry.collections.is_empty() {
        names.push("CollectionSlug".to_string());
    }
    names
}

/// Push one owner's type names into `names`: the owner itself, its sub-types,
/// and any select-enum / polymorphic-enum types its fields declare.
fn collect_type_names(fields: &[FieldDefinition], root: &str, names: &mut Vec<String>) {
    names.push(root.to_string());
    for stf in collect_sub_type_fields(fields, root) {
        let sub = format!("{}{}", stf.parent_pascal, to_pascal_case(&stf.field.name));
        push_aux_type_names(&resolve_fields(&stf.field.fields, &sub), names);
        names.push(sub);
    }
    push_aux_type_names(&resolve_fields(fields, root), names);
}

/// Push the named auxiliary types (select enum, polymorphic wrapper + `…Ref`).
fn push_aux_type_names(fields: &[Field], names: &mut Vec<String>) {
    for f in fields {
        match &f.ty {
            FieldTy::Enum { name, .. } => names.push(name.clone()),
            FieldTy::PolyRel { name, .. } => {
                names.push(name.clone());
                names.push(format!("{name}Ref"));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod collision {
    use crate::core::{
        CollectionDefinition, FieldDefinition, FieldType, LocalizedString, Registry, SelectOption,
    };
    use crate::typegen::Language;

    use super::generate;

    #[test]
    fn errors_when_two_constructs_produce_the_same_type_name() {
        let mut reg = Registry::new();
        // A collection `posts_status` → `struct PostsStatus`…
        reg.register_collection(CollectionDefinition::new("posts_status"));
        // …collides with a `posts.status` select → `enum PostsStatus`.
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("status", FieldType::Select)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("A".into()),
                    "a",
                )])
                .build(),
        ];
        reg.register_collection(posts);

        let err = generate(&reg, Language::Rust).unwrap_err().to_string();
        assert!(
            err.contains("PostsStatus"),
            "err names the collision: {err}"
        );
    }

    #[test]
    fn empty_registry_and_no_collision_are_ok() {
        assert!(generate(&Registry::new(), Language::Rust).is_ok());
        assert!(generate(&Registry::new(), Language::Typescript).is_ok());
        assert!(generate(&Registry::new(), Language::Go).is_ok());
        assert!(generate(&Registry::new(), Language::Python).is_ok());

        let mut reg = Registry::new();
        reg.register_collection(CollectionDefinition::new("posts"));
        assert!(generate(&reg, Language::Rust).is_ok());
    }
}

/// Golden-snapshot tests: one comprehensive schema rendered per language and
/// diffed against a committed file, so any output change fails until reviewed —
/// the "can't regress silently" net. This complements the per-language unit
/// tests (behavioral intent) and the Rust `syn` parse (compile-grade validity);
/// a true TS/Go/Python compiler check would need those toolchains, out of scope
/// for the hermetic Rust suite. Regenerate the goldens after an intentional
/// change with: `cargo test -p crap-cms --lib golden::regenerate -- --ignored`.
#[cfg(test)]
mod golden {
    use crate::core::{
        BlockDefinition, CollectionDefinition, FieldDefinition, FieldType, GlobalDefinition,
        LocalizedString, Registry, RelationshipConfig, SelectOption,
    };
    use crate::typegen::Language;

    use super::generate;

    fn text(name: &str, required: bool) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .required(required)
            .build()
    }

    /// A schema exercising the breadth of the generators: scalars, has-many,
    /// select-with-options, populated + polymorphic relationships, upload,
    /// group, nested group in array, blocks, a global, and identifier hazards
    /// (a leading-digit slug/field, a keyword field).
    fn kitchen_sink() -> Registry {
        let mut posts = CollectionDefinition::new("posts");
        posts.timestamps = true;
        posts.fields = vec![
            text("title", true),
            FieldDefinition::builder("status", FieldType::Select)
                .required(true)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Draft".into()), "draft"),
                    SelectOption::new(LocalizedString::Plain("Published".into()), "published"),
                ])
                .build(),
            FieldDefinition::builder("author", FieldType::Relationship)
                .required(true)
                .relationship(RelationshipConfig::new("users", false))
                .build(),
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
            FieldDefinition::builder("cover", FieldType::Upload)
                .relationship(RelationshipConfig::new("media", false))
                .build(),
            FieldDefinition::builder("related", FieldType::Relationship)
                .relationship({
                    // Both targets are registered below, so the golden compiles.
                    let mut rc = RelationshipConfig::new("users", true);
                    rc.polymorphic = vec!["users".into(), "tags".into()];
                    rc
                })
                .build(),
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![text("meta_title", true), text("meta_desc", false)])
                .build(),
            FieldDefinition::builder("items", FieldType::Array)
                .fields(vec![
                    text("label", true),
                    FieldDefinition::builder("meta", FieldType::Group)
                        .fields(vec![text("key", true)])
                        .build(),
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
            FieldDefinition::builder("scores", FieldType::Number)
                .has_many(true)
                .build(),
            FieldDefinition::builder("active", FieldType::Checkbox).build(),
            FieldDefinition::builder("data", FieldType::Json).build(),
        ];

        let mut users = CollectionDefinition::new("users");
        users.timestamps = true;
        users.fields = vec![
            text("name", true),
            FieldDefinition::builder("email", FieldType::Email).build(),
        ];

        let mut tags = CollectionDefinition::new("tags");
        tags.fields = vec![text("name", true)];

        let mut media = CollectionDefinition::new("media");
        media.fields = vec![text("filename", true)];

        // Identifier hazards: a leading-digit slug + field, and a keyword field.
        let mut twofa = CollectionDefinition::new("2fa");
        twofa.fields = vec![text("type", true), text("2fa", false)];

        let mut settings = GlobalDefinition::new("settings");
        settings.fields = vec![
            text("site_name", true),
            FieldDefinition::builder("nav", FieldType::Array)
                .fields(vec![text("label", true), text("url", true)])
                .build(),
        ];

        let mut reg = Registry::new();
        for c in [posts, users, tags, media, twofa] {
            reg.register_collection(c);
        }
        reg.register_global(settings);
        reg
    }

    const LANGS: [(Language, &str); 4] = [
        (Language::Rust, "rs"),
        (Language::Typescript, "ts"),
        (Language::Go, "go"),
        (Language::Python, "py"),
    ];

    /// Regenerate every golden. Ignored by default (it writes into the source
    /// tree); run explicitly after an intentional generator change.
    #[test]
    #[ignore = "writes golden files into testdata/; run with --ignored to regenerate"]
    fn regenerate() {
        let reg = kitchen_sink();
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src/typegen/client/testdata");
        std::fs::create_dir_all(dir).expect("create testdata dir");
        for (lang, ext) in LANGS {
            let src = generate(&reg, lang).expect("generate golden");
            std::fs::write(format!("{dir}/kitchen_sink.{ext}"), src).expect("write golden");
        }
    }

    macro_rules! golden_test {
        ($name:ident, $lang:expr, $file:literal) => {
            #[test]
            fn $name() {
                let actual = generate(&kitchen_sink(), $lang).expect("generate");
                let expected = include_str!($file);
                assert_eq!(
                    actual,
                    expected,
                    "{} golden is stale — regenerate with \
                     `cargo test -p crap-cms --lib golden::regenerate -- --ignored`",
                    stringify!($name)
                );
            }
        };
    }

    golden_test!(golden_rust, Language::Rust, "testdata/kitchen_sink.rs");
    golden_test!(
        golden_typescript,
        Language::Typescript,
        "testdata/kitchen_sink.ts"
    );
    golden_test!(golden_go, Language::Go, "testdata/kitchen_sink.go");
    golden_test!(golden_python, Language::Python, "testdata/kitchen_sink.py");
}
