//! The shared schema walk: resolve every field to the language-neutral IR
//! ([`super::ir`]) and stream `SubType`/`Document` constructs to a
//! [`ClientPrinter`]. Written once here instead of four times across the
//! per-language backends.
//!
//! Every owner is walked in its two wire shapes (`core::upload::read_shape`):
//! the read shape feeds the read types, the write shape the input (`…Data`)
//! types. Neither is ever derived from the other.

use std::{borrow::Cow, collections::HashSet, slice::from_ref};

use crate::{
    core::{
        CollectionDefinition, FieldChildren, FieldDefinition, FieldType, Registry,
        collection::GlobalDefinition,
        field_children, flatten_array_sub_fields,
        upload::{read_shape_fields, readable_fields, writable_fields, write_shape_fields},
    },
    typegen::{
        Language,
        helpers::{
            COLLECTION_TAG_KEY, DRAFT_STATUS_VALUES, SubTypeKind, collect_sub_type_fields,
            declares_collection_tag, is_optional, rel_has_many, sorted_collection_slugs,
            sorted_global_slugs, to_pascal_case,
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

/// The two wire shapes of one owner's fields: what a read returns and what a
/// write accepts.
#[derive(Clone, Copy)]
struct Shapes<'a> {
    read: &'a [FieldDefinition],
    write: &'a [FieldDefinition],
}

impl<'a> Shapes<'a> {
    fn new(read: &'a [FieldDefinition], write: &'a [FieldDefinition]) -> Self {
        Self { read, write }
    }
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
        let (read, write) = (read_shape_fields(col), write_shape_fields(col));
        let shapes = Shapes::new(&read, &write);

        let doc = collection_document(col, shapes);
        emit_owner_document(printer.as_mut(), &doc, shapes, &mut aux);
    }

    for slug in sorted_global_slugs(registry) {
        let global = &registry.globals[slug];
        let read = readable_fields(&global.fields);
        let write = writable_fields(&global.fields);
        let shapes = Shapes::new(&read, &write);

        let doc = global_document(global, shapes);
        emit_owner_document(printer.as_mut(), &doc, shapes, &mut aux);
    }

    printer.enum_types(&aux.enums);
    printer.poly_types(&aux.polys);
    printer.epilogue(registry);
    printer.finish()
}

/// Emit one collection or global: its input sub-types, its read sub-types
/// (collecting their auxiliary types), its document, then its
/// `locale = "all"` read shape.
fn emit_owner_document<'a>(
    printer: &mut dyn ClientPrinter,
    doc: &Document<'a>,
    shapes: Shapes<'a>,
    aux: &mut Aux,
) {
    for sub in sub_types(shapes.write, &doc.name, Shape::Input) {
        printer.sub_type(&sub);
    }

    for sub in sub_types(shapes.read, &doc.name, Shape::Read) {
        collect_aux(&sub.fields, aux);
        printer.sub_type(&sub);
    }

    collect_aux(&doc.fields, aux);
    printer.document(doc);

    emit_localized(printer, doc, shapes.read);
}

/// The named types collected during the walk (deduped by their unique names).
#[derive(Default)]
struct Aux {
    enums: Vec<EnumDef>,
    polys: Vec<PolyDef>,
    seen: HashSet<String>,
}

/// Every sub-type an owner's `fields` declare, in declaration order, resolved
/// in `shape`.
fn sub_types<'a>(
    fields: &'a [FieldDefinition],
    root_pascal: &str,
    shape: Shape,
) -> Vec<SubType<'a>> {
    collect_sub_type_fields(fields, root_pascal)
        .into_iter()
        .map(|stf| {
            let name = format!("{}{}", stf.parent_pascal, to_pascal_case(&stf.field.name));

            let mut fields = resolve_shaped(&stf.field.fields, &name, shape);
            if stf.row_id {
                fields.insert(0, row_id_field());
            }

            SubType {
                name,
                kind: stf.kind,
                field_name: &stf.field.name,
                fields,
                input: shape.is_input(),
            }
        })
        .collect()
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

/// Build the [`Document`] for a collection from its read and write shapes.
fn collection_document<'a>(col: &'a CollectionDefinition, shapes: Shapes<'a>) -> Document<'a> {
    let root = to_pascal_case(&col.slug);

    Document {
        fields: resolve_fields(shapes.read, &root),
        input: collection_input(col, shapes.write, &root),
        system: system_fields(col.has_drafts(), col.soft_delete),
        collection_tag: collection_tag(&col.slug, shapes.read),
        name: root,
        slug: &col.slug,
        timestamps: col.timestamps,
        is_global: false,
        localized: false,
        select_options: select_field_options(shapes.read),
    }
}

/// Build the [`Document`] for a global (always timestamped, no select
/// docstring, never populated into a relationship).
fn global_document<'a>(global: &'a GlobalDefinition, shapes: Shapes<'a>) -> Document<'a> {
    let root = to_pascal_case(&global.slug);

    Document {
        fields: resolve_fields(shapes.read, &root),
        input: resolve_shaped(shapes.write, &root, Shape::Input),
        system: system_fields(global.has_drafts(), false),
        collection_tag: None,
        name: root,
        slug: &global.slug,
        timestamps: true,
        is_global: true,
        localized: false,
        select_options: Vec::new(),
    }
}

/// A collection's write shape resolved for input, plus the `password` an auth
/// collection's create and update take beside its fields.
fn collection_input<'a>(
    col: &CollectionDefinition,
    write: &'a [FieldDefinition],
    root: &str,
) -> Vec<Field<'a>> {
    let mut input = resolve_shaped(write, root, Shape::Input);

    if col.is_auth_collection() && !input.iter().any(|f| f.name == "password") {
        input.push(password_field());
    }

    input
}

/// An auth collection's `password`: extracted from the data by create and
/// single update, hashed, and never stored as field data or read back.
/// Optional — an account may authenticate by another method; an empty one is
/// rejected on create and keeps the stored hash on update.
fn password_field() -> Field<'static> {
    Field {
        name: Cow::Borrowed("password"),
        ty: FieldTy::Str,
        optional: true,
    }
}

/// The `collection` key a populated copy of a document carries — its slug —
/// unless one of its own fields has that name.
fn collection_tag(slug: &str, read: &[FieldDefinition]) -> Option<Field<'static>> {
    if !declares_collection_tag(read) {
        return None;
    }

    Some(Field {
        name: Cow::Borrowed(COLLECTION_TAG_KEY),
        ty: FieldTy::Literal(vec![slug.to_string()]),
        optional: true,
    })
}

/// The stored keys a read document carries besides its fields.
fn system_fields(drafts: bool, soft_delete: bool) -> Vec<Field<'static>> {
    let mut fields = Vec::new();

    if drafts {
        fields.push(Field {
            name: Cow::Borrowed("_status"),
            ty: FieldTy::Literal(DRAFT_STATUS_VALUES.into_iter().map(String::from).collect()),
            optional: true,
        });
    }

    if soft_delete {
        fields.push(Field {
            name: Cow::Borrowed("_deleted_at"),
            ty: FieldTy::Str,
            optional: true,
        });
    }

    fields
}

/// How a document's fields are typed.
#[derive(Clone, Copy)]
enum Shape {
    /// What a single-locale read returns: every field optional, a reference an
    /// id or its populated document.
    Read,
    /// What a create or update accepts: each field's own optionality, every
    /// reference an id (a polymorphic one as its `"collection/id"` string).
    Input,
    /// The `locale = "all"` read shape; `inherited` is whether an enclosing
    /// group is localized.
    Localized { inherited: bool },
}

impl Shape {
    fn is_input(self) -> bool {
        matches!(self, Shape::Input)
    }

    /// Whether `field` may be left out. A read may omit any field — a draft
    /// may lack required values, population nulls a reference whose target is
    /// gone or denied, and field read access and `select` leave keys out — so
    /// only an input keeps the field's own optionality.
    fn optional(self, field: &FieldDefinition) -> bool {
        !self.is_input() || is_optional(field)
    }

    /// Whether `field` is read as a per-locale map in this shape.
    fn localizes(self, field: &FieldDefinition) -> bool {
        let Shape::Localized { inherited } = self else {
            return false;
        };

        field.has_parent_column() && (inherited || field.localized)
    }

    /// `ty` as this shape types `field`.
    fn shape_ty(self, field: &FieldDefinition, ty: FieldTy, localized: bool) -> FieldTy {
        match self {
            Shape::Read => ty,
            Shape::Input => input_ty(ty),
            Shape::Localized { inherited } => localized_ty(field, ty, localized, inherited),
        }
    }
}

/// A reference as a write carries it: the id string, never a document.
fn input_ty(ty: FieldTy) -> FieldTy {
    match ty {
        FieldTy::Rel { many, .. } | FieldTy::PolyRel { many, .. } => {
            FieldTy::Rel { target: None, many }
        }
        other => other,
    }
}

/// `ty` in the `locale = "all"` shape: a per-locale map for a localized
/// column, the localized sub-type for a group holding localized columns.
fn localized_ty(field: &FieldDefinition, ty: FieldTy, localized: bool, inherited: bool) -> FieldTy {
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
        fields: resolve_shaped(sub, &pascal, shape),
        input: false,
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
        input: Vec::new(),
        system: doc.system.clone(),
        collection_tag: doc.collection_tag.clone(),
        timestamps: doc.timestamps,
        is_global: doc.is_global,
        select_options: Vec::new(),
        localized: true,
    });
}

/// Resolve a field list to the IR's read shape, flattening transparent layout
/// wrappers (Row/Collapsible/Tabs) so a printer never sees them.
/// `parent_pascal` is the enclosing owner's compound `PascalCase`, used to name
/// nested sub-types.
fn resolve_fields<'a>(fields: &'a [FieldDefinition], parent_pascal: &str) -> Vec<Field<'a>> {
    resolve_shaped(fields, parent_pascal, Shape::Read)
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
/// wrapper. A field is followed by its companion keys (a timezone date's
/// `<name>_tz`, a code field's `<name>_lang`), each an optional string.
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
        optional: shape.optional(field),
    });

    for column in field.companion_columns(&field.name) {
        out.push(Field {
            name: Cow::Owned(column),
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
        // A rich text field stored as a JSON document reads parsed, like a
        // `json` field — the same predicate the column decoding uses.
        FieldType::Richtext if field.parses_json() => FieldTy::Json,
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
        let (read, write) = (read_shape_fields(col), write_shape_fields(col));
        let root = to_pascal_case(&col.slug);

        collect_type_names(Shapes::new(&read, &write), &root, lang, &mut names);
    }

    for slug in sorted_global_slugs(registry) {
        let global = &registry.globals[slug];
        let read = readable_fields(&global.fields);
        let write = writable_fields(&global.fields);
        let root = to_pascal_case(&global.slug);

        collect_type_names(Shapes::new(&read, &write), &root, lang, &mut names);
    }

    if !registry.collections.is_empty() {
        names.push("CollectionSlug".to_string());
    }

    names
}

/// Push one owner's type names into `names`: the owner itself, its sub-types,
/// and any select-enum / polymorphic-enum types its fields declare — as `lang`
/// names them. TypeScript names an owner by its input and read types
/// (`…Data`, `…Document`, `…LocalizedDocument`), names each write-shape
/// sub-type `…Data`, and writes select enums and polymorphic references
/// inline, so they name no type there.
fn collect_type_names(shapes: Shapes<'_>, root: &str, lang: Language, names: &mut Vec<String>) {
    let typescript = matches!(lang, Language::Typescript);

    if typescript {
        names.extend([format!("{root}Data"), format!("{root}Document")]);
        names.extend(
            sub_types(shapes.write, root, Shape::Input)
                .into_iter()
                .map(|sub| format!("{}Data", sub.name)),
        );
        names.extend(
            sub_types(shapes.read, root, Shape::Read)
                .into_iter()
                .map(|sub| sub.name),
        );
    } else {
        names.push(root.to_string());

        for sub in sub_types(shapes.read, root, Shape::Read) {
            push_aux_type_names(&sub.fields, names);
            names.push(sub.name);
        }

        push_aux_type_names(&resolve_fields(shapes.read, root), names);
    }

    names.extend(localized_type_names(shapes.read, root, typescript));
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
