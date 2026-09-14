//! The shared schema walk: resolve every field to the language-neutral IR
//! ([`super::ir`]) and stream `SubType`/`Document` constructs to a
//! [`ClientPrinter`]. Written once here instead of four times across the
//! per-language backends.

use std::{borrow::Cow, collections::HashSet, slice::from_ref};

use crate::{
    core::{
        CollectionDefinition, FieldChildren, FieldDefinition, FieldType, Registry,
        collection::GlobalDefinition, field_children, flatten_array_sub_fields,
    },
    db::query::helpers::tz_column,
    typegen::{
        Language,
        helpers::{
            SubTypeKind, collect_sub_type_fields, is_optional, is_single_ref, rel_has_many,
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
    check_type_name_collisions(registry, lang)?;

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
        let owner = (col.fields.as_slice(), to_pascal_case(&col.slug));
        emit_owner_document(printer.as_mut(), owner, &collection_document(col), &mut aux);
    }

    for slug in sorted_global_slugs(registry) {
        let global = &registry.globals[slug];
        let owner = (global.fields.as_slice(), to_pascal_case(&global.slug));
        emit_owner_document(printer.as_mut(), owner, &global_document(global), &mut aux);
    }

    printer.enum_types(&aux.enums);
    printer.poly_types(&aux.polys);
    printer.epilogue(registry);
    printer.finish()
}

/// Emit one collection or global — its fields and the Pascal-case name they are
/// typed under — then its document and `locale = "all"` read shape.
fn emit_owner_document(
    printer: &mut dyn ClientPrinter,
    (fields, pascal): (&[FieldDefinition], String),
    doc: &Document<'_>,
    aux: &mut Aux,
) {
    emit_owner(printer, fields, &pascal, aux);
    collect_aux(&doc.fields, aux);
    printer.document(doc);
    emit_localized(printer, doc, fields);
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
        let mut fields = resolve_fields(&stf.field.fields, &sub_pascal);
        if stf.row_id {
            fields.insert(0, row_id_field());
        }

        let sub = SubType {
            name,
            kind: stf.kind,
            field_name: &stf.field.name,
            fields,
            read_only: false,
        };
        collect_aux(&sub.fields, aux);
        printer.sub_type(&sub);
    }
}

/// The junction `id` a relational array row carries: sent back on update, it
/// keeps the stored row instead of replacing it.
fn row_id_field() -> Field<'static> {
    Field {
        name: Cow::Borrowed("id"),
        ty: FieldTy::Str,
        optional: true,
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
        system: system_fields(col.has_drafts(), col.soft_delete),
        name: root,
        slug: &col.slug,
        timestamps: col.timestamps,
        is_global: false,
        localized: false,
        select_options: select_field_options(&col.fields),
    }
}

/// Build the [`Document`] for a global (always timestamped, no select docstring).
fn global_document(global: &GlobalDefinition) -> Document<'_> {
    let root = to_pascal_case(&global.slug);
    Document {
        fields: resolve_fields(&global.fields, &root),
        system: system_fields(global.has_drafts(), false),
        name: root,
        slug: &global.slug,
        timestamps: true,
        is_global: true,
        localized: false,
        select_options: Vec::new(),
    }
}

/// The stored keys a read document carries besides its fields.
fn system_fields(drafts: bool, soft_delete: bool) -> Vec<Field<'static>> {
    let key = |name: &'static str| Field {
        name: Cow::Borrowed(name),
        ty: FieldTy::Str,
        optional: true,
    };

    let mut fields = Vec::new();
    if drafts {
        fields.push(key("_status"));
    }
    if soft_delete {
        fields.push(key("_deleted_at"));
    }

    fields
}

/// Mark every field optional: a read may omit any of them (a draft may lack
/// required values, and field read access and `select` leave keys out).
fn read_fields(mut fields: Vec<Field<'_>>) -> Vec<Field<'_>> {
    for field in &mut fields {
        field.optional = true;
    }

    fields
}

/// How a document's fields are typed.
#[derive(Clone, Copy)]
enum Shape {
    /// The single-locale shape reads and writes use.
    Standard,
    /// The `locale = "all"` read shape; `inherited` is whether an enclosing
    /// group is localized.
    Localized { inherited: bool },
}

impl Shape {
    /// Whether `field` is read as a per-locale map in this shape.
    fn localizes(self, field: &FieldDefinition) -> bool {
        match self {
            Shape::Standard => false,
            Shape::Localized { inherited } => {
                field.has_parent_column() && (inherited || field.localized)
            }
        }
    }

    /// `ty` as this shape types `field`: a per-locale map for a localized
    /// column, the localized sub-type for a group holding localized columns.
    fn shape_ty(self, field: &FieldDefinition, ty: FieldTy, localized: bool) -> FieldTy {
        let Shape::Localized { inherited } = self else {
            return ty;
        };

        if let FieldTy::SubType { name, list: false } = &ty
            && field.field_type == FieldType::Group
            && has_localized_columns(&field.fields, inherited || field.localized)
        {
            return FieldTy::SubType {
                name: format!("{name}Localized"),
                list: false,
            };
        }

        localized_if(ty, localized)
    }
}

fn localized_if(ty: FieldTy, localized: bool) -> FieldTy {
    if localized {
        FieldTy::Localized(Box::new(ty))
    } else {
        ty
    }
}

/// Whether `fields` hold a per-locale column, looking through layout wrappers
/// and groups. Array and blocks rows are never per-locale columns.
fn has_localized_columns(fields: &[FieldDefinition], inherited: bool) -> bool {
    fields.iter().any(|f| match field_children(f) {
        FieldChildren::Wrapper(sub) => has_localized_columns(sub, inherited),
        FieldChildren::Tabs(tabs) => tabs
            .iter()
            .any(|tab| has_localized_columns(&tab.fields, inherited)),
        FieldChildren::Group(sub) => has_localized_columns(sub, inherited || f.localized),
        FieldChildren::Leaf => f.has_parent_column() && (inherited || f.localized),
        FieldChildren::Array(_) | FieldChildren::Blocks(_) => false,
    })
}

/// The localized sub-types (`<GroupType>Localized`) of every group holding
/// localized columns, nested groups included.
fn localized_sub_types<'a>(
    fields: &'a [FieldDefinition],
    parent_pascal: &str,
    inherited: bool,
) -> Vec<SubType<'a>> {
    let mut out = Vec::new();

    for f in fields {
        match field_children(f) {
            FieldChildren::Wrapper(sub) => {
                out.extend(localized_sub_types(sub, parent_pascal, inherited));
            }
            FieldChildren::Tabs(tabs) => {
                for tab in tabs {
                    out.extend(localized_sub_types(&tab.fields, parent_pascal, inherited));
                }
            }
            FieldChildren::Group(sub) if has_localized_columns(sub, inherited || f.localized) => {
                out.extend(localized_group_sub_types(f, sub, parent_pascal, inherited));
            }
            _ => {}
        }
    }

    out
}

/// The `locale = "all"` sub-type of a group holding localized columns, then
/// those of the groups inside it.
fn localized_group_sub_types<'a>(
    group: &'a FieldDefinition,
    sub: &'a [FieldDefinition],
    parent_pascal: &str,
    inherited: bool,
) -> Vec<SubType<'a>> {
    let scope = inherited || group.localized;
    let pascal = format!("{parent_pascal}{}", to_pascal_case(&group.name));
    let shape = Shape::Localized { inherited: scope };

    let mut out = vec![SubType {
        name: format!("{pascal}Localized"),
        kind: SubTypeKind::Group,
        field_name: &group.name,
        fields: read_fields(resolve_shaped(sub, &pascal, shape)),
        read_only: true,
    }];
    out.extend(localized_sub_types(sub, &pascal, scope));

    out
}

/// Emit the `locale = "all"` read shape of an owner with localized columns:
/// the localized sub-types of the groups holding them, then the document.
fn emit_localized<'a>(
    printer: &mut dyn ClientPrinter,
    doc: &Document<'a>,
    fields: &'a [FieldDefinition],
) {
    if !has_localized_columns(fields, false) {
        return;
    }

    for sub in localized_sub_types(fields, &doc.name, false) {
        printer.sub_type(&sub);
    }

    printer.document(&Document {
        name: doc.name.clone(),
        slug: doc.slug,
        fields: resolve_shaped(fields, &doc.name, Shape::Localized { inherited: false }),
        system: doc.system.clone(),
        timestamps: doc.timestamps,
        is_global: doc.is_global,
        select_options: Vec::new(),
        localized: true,
    });
}

/// Resolve a field list to the IR, flattening transparent layout wrappers
/// (Row/Collapsible/Tabs) so a printer never sees them. `parent_pascal` is the
/// enclosing owner's compound `PascalCase`, used to name nested sub-types.
fn resolve_fields<'a>(fields: &'a [FieldDefinition], parent_pascal: &str) -> Vec<Field<'a>> {
    resolve_shaped(fields, parent_pascal, Shape::Standard)
}

/// [`resolve_fields`] in a given [`Shape`].
fn resolve_shaped<'a>(
    fields: &'a [FieldDefinition],
    parent_pascal: &str,
    shape: Shape,
) -> Vec<Field<'a>> {
    let mut out = Vec::new();
    for field in fields {
        push_resolved(&mut out, field, parent_pascal, shape);
    }
    out
}

/// Resolve one field into `out`, recursing through a transparent layout
/// wrapper. A timezone date is followed by its `<name>_tz` companion.
fn push_resolved<'a>(
    out: &mut Vec<Field<'a>>,
    field: &'a FieldDefinition,
    parent_pascal: &str,
    shape: Shape,
) {
    if field.field_type.is_layout_wrapper() {
        for sub in flatten_array_sub_fields(from_ref(field)) {
            push_resolved(out, sub, parent_pascal, shape);
        }
        return;
    }

    let localized = shape.localizes(field);

    out.push(Field {
        name: Cow::Borrowed(&field.name),
        ty: shape.shape_ty(field, resolve_ty(field, parent_pascal), localized),
        // A single relationship/upload is optional on read even when `required`:
        // population nulls it when the target is soft-deleted or access-denied
        // (has-many drops the entry instead), so a non-optional type would lie.
        optional: is_optional(field) || is_single_ref(field),
    });

    if field.has_tz_companion() {
        out.push(Field {
            name: Cow::Owned(tz_column(&field.name)),
            ty: localized_if(FieldTy::Str, localized),
            optional: true,
        });
    }
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
        FieldType::Select | FieldType::Radio => select_ty(field, parent_pascal),
        FieldType::Upload => upload_ty(field),
        FieldType::Relationship => relationship_ty(field, parent_pascal),
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

/// A select or radio: a named enum of its options, or a plain string without
/// options.
fn select_ty(field: &FieldDefinition, parent_pascal: &str) -> FieldTy {
    if field.options.is_empty() {
        return if field.has_many {
            FieldTy::StrList
        } else {
            FieldTy::Str
        };
    }

    FieldTy::Enum {
        name: format!("{parent_pascal}{}", to_pascal_case(&field.name)),
        values: field.options.iter().map(|o| o.value.clone()).collect(),
        many: field.has_many,
    }
}

/// An upload: a reference to its upload collection, when it names one.
fn upload_ty(field: &FieldDefinition) -> FieldTy {
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

/// A relationship: a named union over a polymorphic one's targets, else a
/// reference to its collection.
fn relationship_ty(field: &FieldDefinition, parent_pascal: &str) -> FieldTy {
    match &field.relationship {
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
fn check_type_name_collisions(registry: &Registry, lang: Language) -> anyhow::Result<()> {
    let mut seen = HashSet::new();
    for name in all_type_names(registry, lang) {
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

/// Every top-level type name the generator would emit for `lang`, in walk order
/// (with duplicates preserved so [`check_type_name_collisions`] can spot them).
fn all_type_names(registry: &Registry, lang: Language) -> Vec<String> {
    let mut names = Vec::new();

    for slug in sorted_collection_slugs(registry) {
        let col = &registry.collections[slug];
        collect_type_names(&col.fields, &to_pascal_case(&col.slug), lang, &mut names);
    }
    for slug in sorted_global_slugs(registry) {
        let global = &registry.globals[slug];
        collect_type_names(
            &global.fields,
            &to_pascal_case(&global.slug),
            lang,
            &mut names,
        );
    }
    if !registry.collections.is_empty() {
        names.push("CollectionSlug".to_string());
    }
    names
}

/// Push one owner's type names into `names`: the owner itself, its sub-types,
/// and any select-enum / polymorphic-enum types its fields declare — as `lang`
/// names them. TypeScript names an owner by its input and read types
/// (`…Data`, `…Document`, `…LocalizedDocument`) and gives each sub-type an
/// input variant (`…Data`).
fn collect_type_names(
    fields: &[FieldDefinition],
    root: &str,
    lang: Language,
    names: &mut Vec<String>,
) {
    let typescript = matches!(lang, Language::Typescript);

    if typescript {
        names.extend([format!("{root}Data"), format!("{root}Document")]);
    } else {
        names.push(root.to_string());
    }

    // TypeScript writes select enums and polymorphic references inline, so
    // they name no type there.
    for stf in collect_sub_type_fields(fields, root) {
        let sub = format!("{}{}", stf.parent_pascal, to_pascal_case(&stf.field.name));
        if typescript {
            names.push(format!("{sub}Data"));
        } else {
            push_aux_type_names(&resolve_fields(&stf.field.fields, &sub), names);
        }
        names.push(sub);
    }
    if !typescript {
        push_aux_type_names(&resolve_fields(fields, root), names);
    }

    names.extend(localized_type_names(fields, root, typescript));
}

/// The `locale = "all"` read type names of an owner with localized columns:
/// its document, then the localized sub-types of its groups.
fn localized_type_names(fields: &[FieldDefinition], root: &str, typescript: bool) -> Vec<String> {
    if !has_localized_columns(fields, false) {
        return Vec::new();
    }

    let document = if typescript {
        format!("{root}LocalizedDocument")
    } else {
        format!("{root}Localized")
    };

    let mut names = vec![document];
    names.extend(
        localized_sub_types(fields, root, false)
            .into_iter()
            .map(|sub| sub.name),
    );

    names
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

    /// Regression: TypeScript names a localized document `…LocalizedDocument`,
    /// a name the collision check didn't know, so a group field producing the
    /// same name silently merged into it.
    #[test]
    fn typescript_document_names_take_part_in_the_collision_check() {
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .localized(true)
                .build(),
            FieldDefinition::builder("localized_document", FieldType::Group)
                .fields(vec![FieldDefinition::builder("x", FieldType::Text).build()])
                .build(),
        ];
        let mut reg = Registry::new();
        reg.register_collection(posts);

        let err = generate(&reg, Language::Typescript)
            .unwrap_err()
            .to_string();
        assert!(err.contains("PostsLocalizedDocument"), "{err}");
        assert!(
            generate(&reg, Language::Rust).is_ok(),
            "Rust names the localized document `PostsLocalized`"
        );
    }

    /// Regression: the TypeScript collision check counted the select-enum and
    /// polymorphic type names the other languages emit, though TypeScript
    /// writes those inline — so a field named `document` collided with the
    /// `…Document` read type that is the only `PostsDocument` it emits.
    #[test]
    fn typescript_inline_enums_name_no_type() {
        let mut posts = CollectionDefinition::new("posts");
        posts.fields = vec![
            FieldDefinition::builder("document", FieldType::Select)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("A".into()),
                    "a",
                )])
                .build(),
        ];
        let mut reg = Registry::new();
        reg.register_collection(posts);

        assert!(generate(&reg, Language::Typescript).is_ok());
        assert!(generate(&reg, Language::Rust).is_ok());
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
        LocalizedString, Registry, RelationshipConfig, SelectOption, VersionsConfig,
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
    /// group, nested group in array, blocks, a global, identifier hazards
    /// (a leading-digit slug/field, a keyword field), drafts, soft delete, a
    /// timezone date, and a localized field and group.
    fn kitchen_sink() -> Registry {
        let mut posts = CollectionDefinition::new("posts");
        posts.timestamps = true;
        posts.soft_delete = true;
        posts.versions = Some(VersionsConfig::new(true, 10));
        posts.fields = vec![
            text("title", true),
            FieldDefinition::builder("summary", FieldType::Textarea)
                .localized(true)
                .build(),
            FieldDefinition::builder("published_at", FieldType::Date)
                .timezone(true)
                .build(),
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
                .localized(true)
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
