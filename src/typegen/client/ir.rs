//! The language-neutral intermediate representation the shared schema walk
//! produces, plus the [`ClientPrinter`] trait every backend implements. The
//! walk that builds these lives in [`super::driver`]; the per-language rendering
//! lives in the printer modules.

use std::borrow::Cow;

use crate::{core::Registry, typegen::helpers::SubTypeKind};

/// A field's language-neutral type, resolved once from the schema. Each printer
/// maps it to its own syntax. A read relationship is an id-or-document union
/// ([`FieldTy::Rel`] with a target, [`FieldTy::PolyRel`]); a write carries ids
/// only, so the input shape resolves every reference to `Rel { target: None }`.
#[derive(Clone)]
pub(in crate::typegen) enum FieldTy {
    /// A single string (`Text`/`Textarea`/`Email`/`Date`/`Richtext`/`Code`).
    Str,
    /// A single float (`Number`).
    Num,
    /// A boolean (`Checkbox`).
    Bool,
    /// Arbitrary JSON with no shape (`Json`) — `interface{}` / `Any` /
    /// `unknown` / `serde_json::Value`.
    Json,
    /// A JSON *object* of unknown shape (empty `Group`) — `map[string]interface{}`
    /// / `dict` / `Record<string, unknown>` / `serde_json::Value`. Distinct from
    /// [`FieldTy::Json`] in every language except Rust.
    Map,
    /// A list of strings (has-many `Text`, id-string relationship lists).
    StrList,
    /// A list of floats (has-many `Number`).
    NumList,
    /// A list of arbitrary JSON objects (`Blocks`, `Join`, empty `Array`).
    JsonList,
    /// A single-target relationship/upload. `target` is the raw `PascalCase`
    /// target collection name (populated document type), or `None` for a plain
    /// id string (empty-collection upload / relationship with no config, and
    /// every reference in the write shape — a polymorphic one as its
    /// `"collection/id"` string). At
    /// `depth=0` the wire value is the id string, at `depth>=1` the document —
    /// modeled as an id-or-doc union in every language.
    Rel { target: Option<String>, many: bool },
    /// A polymorphic relationship. `targets` are the raw target collection slugs
    /// (for the discriminated union / comment); `name` is the raw compound
    /// `PascalCase` of the generated Rust wrapper enum. Unpopulated it is a
    /// `"collection/id"` string; at `depth>=1` it is a `{collection, ...doc}`
    /// object, so it must be typed as id-or-(one of the target docs), never a
    /// plain string.
    PolyRel {
        name: String,
        targets: Vec<String>,
        many: bool,
    },
    /// A named sub-type: `name` is the raw compound `PascalCase` (e.g.
    /// `PostsItems`); `list` distinguishes Array (`true`) from Group (`false`).
    SubType { name: String, list: bool },
    /// A `Select`/`Radio` with explicit options. `name` is the raw compound
    /// `PascalCase` of the generated enum type (`PostsStatus`); TS inlines a
    /// string-literal union and Python a `Literal` (both ignore `name`), while
    /// Rust and Go reference `name` and emit the type via [`ClientPrinter::enum_types`].
    Enum {
        name: String,
        values: Vec<String>,
        many: bool,
    },
    /// A localized column field read with `locale = "all"`: one value per
    /// locale code.
    Localized(Box<FieldTy>),
    /// A string the server writes from a closed set (a system key such as
    /// `_status`, or the `collection` tag of a populated document).
    /// TypeScript and Python narrow to the literal values; Rust and Go keep a
    /// plain string.
    Literal(Vec<String>),
}

/// A named enum type generated from a `Select`/`Radio` field's options, emitted
/// once at package/module level by the languages that reference it by name.
pub(in crate::typegen) struct EnumDef {
    /// Raw compound `PascalCase` name (e.g. `PostsStatus`, `PostsItemsStatus`).
    pub name: String,
    /// The raw option values, in declaration order.
    pub values: Vec<String>,
}

/// A polymorphic-relationship type generated for the languages (Rust) that
/// reference it by name — an id string or one of the target documents, keyed by
/// the `collection` discriminator.
pub(in crate::typegen) struct PolyDef {
    /// Raw compound `PascalCase` name (e.g. `PostsRelated`).
    pub name: String,
    /// The raw target collection slugs (discriminator values).
    pub targets: Vec<String>,
}

/// One resolved field, ready for a printer to render.
#[derive(Clone)]
pub(in crate::typegen) struct Field<'a> {
    /// The raw wire key: a schema field name, or a synthesized companion key
    /// such as a timezone date's `<name>_tz` or a code field's `<name>_lang`.
    /// Printers sanitize per language.
    pub name: Cow<'a, str>,
    pub ty: FieldTy,
    pub optional: bool,
}

/// A named sub-type generated from a non-empty Array or Group field.
pub(in crate::typegen) struct SubType<'a> {
    /// Raw compound `PascalCase` name (e.g. `PostsItems`, `PostsItemsMeta`).
    pub name: String,
    pub kind: SubTypeKind,
    /// The raw field name, for languages that describe the sub-type in a comment.
    pub field_name: &'a str,
    /// The sub-type's fields in its shape: an input sub-type keeps each
    /// field's own optionality and carries references as ids; a read sub-type
    /// makes every field optional.
    pub fields: Vec<Field<'a>>,
    /// Part of the write shape (a `…Data` type). Only the printers that emit
    /// write types render it; the others skip it.
    pub input: bool,
}

/// A top-level document type (a collection document or a global).
pub(in crate::typegen) struct Document<'a> {
    /// Raw `PascalCase` name (e.g. `Posts`, `SiteSettings`).
    pub name: String,
    /// The raw slug, for languages that describe the document in a comment.
    pub slug: &'a str,
    /// The fields a read returns (the read shape), every one optional.
    pub fields: Vec<Field<'a>>,
    /// The fields a create or update accepts (the write shape), each with its
    /// own optionality and references as ids — plus an auth collection's
    /// `password`. Empty for the `locale = "all"` read shape.
    pub input: Vec<Field<'a>>,
    /// Stored keys a read document carries besides its fields (`_status`,
    /// `_deleted_at`) — always optional, never part of the input.
    pub system: Vec<Field<'a>>,
    /// The `collection` key a copy of this document carries when it is
    /// populated into a relationship, typed as the one-value literal of its
    /// slug — what narrows a polymorphic union. `None` for a global (never a
    /// relationship target) and for a collection whose own field is named
    /// `collection`. Rust omits it: its polymorphic enums consume the key as
    /// their serde tag.
    pub collection_tag: Option<Field<'a>>,
    /// Whether to emit `created_at`/`updated_at` (globals always do).
    pub timestamps: bool,
    pub is_global: bool,
    /// The `locale = "all"` read shape: localized fields are per-locale maps, and
    /// a printer emits only a read type for it.
    pub localized: bool,
    /// Top-level `Select` fields with options as `(raw_name, raw_values)`, for
    /// languages that document them (Python). Empty for globals.
    pub select_options: Vec<(String, Vec<String>)>,
}

/// A per-language emitter. Each method renders one construct; balanced blocks
/// and indentation are the printer's responsibility (via `super::writer`).
pub(in crate::typegen) trait ClientPrinter {
    /// File header + any language preamble (imports, the Rust `Rel<T>` enum, …).
    fn prelude(&mut self);
    /// A named sub-type definition.
    fn sub_type(&mut self, def: &SubType);
    /// A top-level document/global type.
    fn document(&mut self, def: &Document);
    /// The named enum types (from `Select`/`Radio` options), emitted once after
    /// the structs. TypeScript and Python inline their narrowing and no-op here.
    fn enum_types(&mut self, defs: &[EnumDef]);
    /// The named polymorphic-relationship types, emitted once after the structs.
    /// TypeScript, Python, and Go inline (or erase) these and no-op here; only
    /// Rust generates the discriminated enum.
    fn poly_types(&mut self, defs: &[PolyDef]);
    /// Trailing output after all types (e.g. the TS `CollectionSlug` union).
    fn epilogue(&mut self, registry: &Registry);
    /// Consume the printer and return the accumulated source.
    fn finish(self: Box<Self>) -> String;
}
