//! Per-type traits + derives for emitting `lua-language-server` type
//! annotations from Rust source.
//!
//! Two derives, two traits:
//!
//! - [`LuaAnnotation`] — for named-fields structs that mirror a Lua
//!   `--- @class crap.X` block. Derive walks the struct's fields, maps
//!   each Rust type to a `LuaLS` type string, and emits the class block.
//! - [`LuaAlias`] — for unit-variant enums that mirror a Lua
//!   `--- @alias crap.X "a" | "b"` union. Derive walks variants and emits
//!   the union literal.
//!
//! Both derives forward Rust `///` doc-comments into the generated Lua
//! output as `--- ` prose, so documentation lives next to the type and is
//! one source of truth.
//!
//! Re-exported from `crate::typegen::lua` so call sites get trait + derive
//! via the same module path.

/// A Rust type that has a corresponding `lua-language-server` class
/// annotation. Implementors emit a `--- @class <name>` block + one
/// `--- @field <name> <type>` line per public field, followed by a blank
/// line so the next annotation reads as its own paragraph.
///
/// The [`CLASS_NAME`](Self::CLASS_NAME) const lets derived impls reference
/// each other's class strings at use-site (e.g. a `Vec<NavCollection>`
/// field can compose `<NavCollection as LuaAnnotation>::CLASS_NAME + "[]"`
/// without the macro needing to track sibling paths).
pub trait LuaAnnotation {
    /// The `--- @class` name emitted for this type
    /// (e.g. `"crap.template.nav_collection"`). Used by other types that
    /// reference this one as a field type.
    const CLASS_NAME: &'static str;

    /// Append this type's `--- @class` block to `out`.
    fn render_lua_annotation(out: &mut String);
}

/// A Rust unit-variant enum that has a corresponding `lua-language-server`
/// type alias. Implementors emit a `--- @alias <name>` block listing one
/// literal per variant.
///
/// The [`ALIAS_NAME`](Self::ALIAS_NAME) const is the alias's full Lua name
/// (e.g. `"crap.FieldType"`), parallel to [`LuaAnnotation::CLASS_NAME`].
pub trait LuaAlias {
    /// The `--- @alias` name emitted for this enum
    /// (e.g. `"crap.FieldType"`).
    const ALIAS_NAME: &'static str;

    /// Append this enum's `--- @alias` block to `out`.
    fn render_lua_alias(out: &mut String);
}

/// Pairs each discriminator variant with the Lua class of its per-field-type
/// view (e.g. `crap.FieldType::Text` → `"crap.TextField"`). Emitted as a
/// side-impl by `#[derive(LuaAlias)]` when any variant carries
/// `#[lua(view_class = "...")]`.
///
/// Consumed by `#[derive(LuaFieldTypeViews)]` on the parent struct
/// (e.g. `FieldDefinition`) to drive per-view class emission. `VIEWS`
/// preserves enum variant order and omits any variant without
/// `view_class` — that's the opt-out signal for variants whose Lua
/// shape can't be auto-derived.
pub trait LuaFieldTypeViewsDiscriminator {
    /// `&[(slug, class_name)]` — same slug strings used in field-level
    /// `#[lua(applies_to = "...")]` attributes on the parent struct.
    const VIEWS: &'static [(&'static str, &'static str)];
}

/// A Rust struct whose `--- @field` lines can be inlined into another
/// `--- @class` block (no class header of its own). Implemented
/// alongside [`LuaAnnotation`] by the same derive — every type that
/// has a class block also exposes its raw field block for flattening.
///
/// Consumers: `#[lua(flatten)]` on a field whose inner type implements
/// this trait — `LuaFieldTypeViews` calls `render_lua_fields_only` to
/// inject the inner type's fields directly into the matching per-view
/// subclass instead of emitting a `--- @field name? T` line.
pub trait LuaFieldBlock {
    /// Append just the `--- @field` lines for this struct, one per line.
    /// No class header, no leading or trailing blank line.
    fn render_lua_fields_only(out: &mut String);
}

/// A Rust struct whose field-by-field shape varies per discriminator
/// variant — typically a "catch-all" type like `FieldDefinition` that
/// stores all possible per-type knobs but only a subset apply per
/// variant. The derive emits a `--- @class crap.BaseField` block with
/// the common fields plus one `--- @class crap.XField : crap.BaseField`
/// per discriminator variant, where each subclass carries only the
/// fields whose `#[lua(applies_to = "...")]` includes that variant's
/// slug.
pub trait LuaFieldTypeViews {
    /// Append the `BaseField` block + every per-variant subclass block
    /// to `out`. One call emits the entire field-type-view section.
    fn render_lua_field_type_views(out: &mut String);
}

pub use crap_cms_macros::{
    LuaAlias, LuaAnnotation, LuaFieldTypeViews, LuaTaggedClass, LuaTypeAlias, lua_fn, lua_table,
};

#[cfg(test)]
mod tests {
    use super::{LuaAlias, LuaAnnotation};

    // ── LuaAnnotation: full type-mapping coverage ──────────────────

    #[derive(LuaAnnotation)]
    #[lua(class = "test.nested")]
    #[allow(dead_code)]
    struct Nested {
        name: String,
    }

    /// Top-level doc-comment on a struct becomes prose above the class.
    /// Second line of the doc spans into the same block.
    #[derive(LuaAnnotation)]
    #[lua(class = "test.fixture")]
    #[allow(dead_code)]
    struct Fixture {
        /// The s field is a string.
        s: String,
        b: bool,
        n: u32,
        f: f64,
        opt: Option<String>,
        vs: Vec<String>,
        nested: Nested,
        nested_opt: Option<Nested>,
        nested_vec: Vec<Nested>,
        #[lua(optional)]
        forced_opt: Vec<String>,
        #[lua(ty = "any")]
        custom: String,
        #[lua(ty_expr = "format!(\"{}|null\", \"string\")")]
        dynamic: &'static str,
        #[lua(rename = "totalDocs")]
        total_docs: u64,
        #[lua(skip)]
        _hidden: bool,
    }

    #[test]
    fn derive_emits_expected_lines_with_docs_and_rename() {
        let mut out = String::new();
        Fixture::render_lua_annotation(&mut out);

        let expected = concat!(
            "--- Top-level doc-comment on a struct becomes prose above the class.\n",
            "--- Second line of the doc spans into the same block.\n",
            "--- @class test.fixture\n",
            "--- @field s string The s field is a string.\n",
            "--- @field b boolean\n",
            "--- @field n integer\n",
            "--- @field f number\n",
            "--- @field opt? string\n",
            "--- @field vs string[]\n",
            "--- @field nested test.nested\n",
            "--- @field nested_opt? test.nested\n",
            "--- @field nested_vec test.nested[]\n",
            "--- @field forced_opt? string[]\n",
            "--- @field custom any\n",
            "--- @field dynamic string|null\n",
            "--- @field totalDocs integer\n",
            "\n",
        );
        assert_eq!(out, expected, "derive output drift:\n{out}");
    }

    #[test]
    fn class_name_const_is_set() {
        assert_eq!(Fixture::CLASS_NAME, "test.fixture");
        assert_eq!(Nested::CLASS_NAME, "test.nested");
    }

    // ── LuaAnnotation: extends (inheritance) ───────────────────────

    #[derive(LuaAnnotation)]
    #[lua(class = "test.base")]
    #[allow(dead_code)]
    struct Base {
        common: String,
    }

    #[derive(LuaAnnotation)]
    #[lua(class = "test.derived", extends = "test.base")]
    #[allow(dead_code)]
    struct Derived {
        extra: bool,
    }

    #[test]
    fn extends_emits_colon_separated_parent() {
        let mut out = String::new();
        Derived::render_lua_annotation(&mut out);
        let expected = concat!(
            "--- @class test.derived : test.base\n",
            "--- @field extra boolean\n",
            "\n",
        );
        assert_eq!(out, expected);
    }

    // ── LuaAlias: enum coverage ────────────────────────────────────

    /// Supported field types for the test fixture.
    #[derive(LuaAlias)]
    #[lua(alias = "test.field_type", rename_all = "snake_case")]
    #[allow(dead_code)]
    enum TestFieldType {
        /// Single-line string.
        Text,
        /// Multi-line text.
        Textarea,
        /// Integer or float.
        Number,
        #[lua(rename = "richtext")]
        RichText,
    }

    #[test]
    fn alias_emits_union_with_variant_docs() {
        // Variant lengths quoted: "text"=6, "textarea"=10, "number"=8,
        // "richtext"=10. Max width = 10. Each shorter variant pads to
        // that width, then a single space separates the variant column
        // from the `#` comment column — so `#` always lands at column
        // (max + 1).
        let mut out = String::new();
        TestFieldType::render_lua_alias(&mut out);
        let expected = concat!(
            "--- Supported field types for the test fixture.\n",
            "--- @alias test.field_type\n",
            "--- | \"text\"     # Single-line string.\n",
            "--- | \"textarea\" # Multi-line text.\n",
            "--- | \"number\"   # Integer or float.\n",
            "--- | \"richtext\"\n",
            "\n",
        );
        assert_eq!(out, expected, "alias output drift:\n{out}");
    }

    #[test]
    fn alias_name_const_is_set() {
        assert_eq!(TestFieldType::ALIAS_NAME, "test.field_type");
    }

    // ── LuaAlias: rename_all strategies ────────────────────────────

    #[derive(LuaAlias)]
    #[lua(alias = "test.kebab", rename_all = "kebab-case")]
    #[allow(dead_code)]
    enum KebabEnum {
        FooBar,
        BazQux,
    }

    #[test]
    fn kebab_case_rename() {
        let mut out = String::new();
        KebabEnum::render_lua_alias(&mut out);
        assert!(out.contains("\"foo-bar\""), "got: {out}");
        assert!(out.contains("\"baz-qux\""), "got: {out}");
    }

    #[derive(LuaAlias)]
    #[lua(alias = "test.lower", rename_all = "lowercase")]
    #[allow(dead_code)]
    enum LowerEnum {
        Foo,
        Bar,
    }

    #[test]
    fn lowercase_rename() {
        let mut out = String::new();
        LowerEnum::render_lua_alias(&mut out);
        assert!(out.contains("\"foo\""), "got: {out}");
        assert!(out.contains("\"bar\""), "got: {out}");
    }

    // ── LuaAlias: type-union (newtype variants) ────────────────────

    /// A value that's either a plain string or a per-locale map.
    /// Plain: just a string.
    /// Localized: `{ en = "...", de = "..." }`.
    #[derive(LuaAlias)]
    #[lua(alias = "test.localized")]
    #[allow(dead_code)]
    enum TestLocalized {
        Plain(String),
        Localized(std::collections::HashMap<String, String>),
    }

    #[test]
    fn type_union_emits_single_line_alias() {
        let mut out = String::new();
        TestLocalized::render_lua_alias(&mut out);
        let expected = concat!(
            "--- A value that's either a plain string or a per-locale map.\n",
            "--- Plain: just a string.\n",
            "--- Localized: `{ en = \"...\", de = \"...\" }`.\n",
            "--- @alias test.localized string | table<string, string>\n",
            "\n",
        );
        assert_eq!(out, expected, "type-union alias drift:\n{out}");
    }
}
