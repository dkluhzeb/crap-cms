//! The language-neutral intermediate representation the shared schema walk
//! produces, plus the [`ClientPrinter`] trait every backend implements. The
//! walk that builds these lives in [`super::driver`]; the per-language rendering
//! lives in the printer modules.

use crate::{core::Registry, typegen::helpers::SubTypeKind};

/// A field's language-neutral type, resolved once from the schema. Each printer
/// maps it to its own syntax — only [`FieldTy::Rel`] carries a populated target
/// (Rust `Rel<T>`); the other languages render relationships as id strings.
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
    /// id string (empty-collection upload / relationship with no config). At
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
pub(in crate::typegen) struct Field<'a> {
    /// The raw schema field name (the wire key). Printers sanitize per language.
    pub name: &'a str,
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
    pub fields: Vec<Field<'a>>,
}

/// A top-level document type (a collection document or a global).
pub(in crate::typegen) struct Document<'a> {
    /// Raw `PascalCase` name (e.g. `Posts`, `SiteSettings`).
    pub name: String,
    /// The raw slug, for languages that describe the document in a comment.
    pub slug: &'a str,
    pub fields: Vec<Field<'a>>,
    /// Whether to emit `created_at`/`updated_at` (globals always do).
    pub timestamps: bool,
    pub is_global: bool,
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
