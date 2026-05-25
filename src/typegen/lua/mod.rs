//! `LuaLS` type definition generator for `types/crap.lua`.
//!
//! ## Two pipelines
//!
//! - **Static file** ([`render_static_file`]) assembles the shipped
//!   `types/crap.lua` from derive-emitted blocks. Composed in
//!   [`static_file`]. Every class / alias / function block flows from
//!   a Rust derive (`LuaAnnotation`, `LuaAlias`, `LuaTypeAlias`,
//!   `LuaFieldTypeViews`) or a `lua_table!`-generated `render_X_lua`
//!   fn — no hand-written `.lua` block files.
//! - **Runtime per-collection** ([`render::render`]) renders a single
//!   collection's definition table on demand (used by the admin
//!   live-reload path). Lives in [`render`] + [`field`]. Uses the
//!   hand-written `field_to_lua_type` mapping in `field.rs` rather
//!   than the derive surface — kept separate to avoid pulling
//!   proc-macros into the request hot path. See the docstring at the
//!   top of `field.rs` for the drift-risk note.
//!
//! ## Derive surface
//!
//! All derives live in the `crap-cms-macros` crate; trait definitions
//! they target are in [`annotation`].
//!
//! | Trait                                  | Apply to              | Emits                              |
//! |----------------------------------------|-----------------------|------------------------------------|
//! | [`LuaAnnotation`]                      | named-fields struct   | `--- @class crap.X` block          |
//! | [`LuaAlias`]                           | unit- or newtype-enum | `--- @alias crap.X` block          |
//! | `LuaTypeAlias`                         | unit struct           | `--- @alias crap.X <literal>`      |
//! | [`LuaFieldTypeViews`]                  | discriminated union   | base + per-variant subclasses      |
//! | [`LuaFieldBlock`]                      | (auto, with above)    | flatten-target fields-only renders |
//! | [`LuaFieldTypeViewsDiscriminator`]     | (auto, on enum)       | `VIEWS: &[(slug, class)]` const    |
//!
//! ## Function macros
//!
//! - `#[lua_fn(path, returns?, returns_doc?)]` (attribute) — registers a
//!   Rust fn as a Lua callable. Generates a `register_*` closure and a
//!   `*_SPEC` const for the typegen pipeline.
//! - `lua_table! { name, path, state, fns, header? }` (function-like)
//!   — declares the path's runtime table + the `render_*_lua` typegen
//!   entry point. The optional `header` emits the namespace
//!   preamble (`-- ── crap.X ──` divider + intro doc + `--- @class
//!   crap.X` + `crap.X = {}`).

mod annotation;
mod ensure_table;
mod field;
mod fn_macro_tests;
mod fn_render;
mod fn_spec;
mod render;
mod sentinel_extract;
mod static_file;

pub use annotation::{
    LuaAlias, LuaAnnotation, LuaFieldBlock, LuaFieldTypeViews, LuaFieldTypeViewsDiscriminator,
    LuaTaggedClass, LuaTypeAlias, lua_fn, lua_table,
};
pub use ensure_table::ensure_table;
pub use fn_render::{format_lua_fn_spec, format_lua_section_header};
pub use fn_spec::{LuaFnSpec, LuaParam, LuaReturn};
pub(super) use render::render;
pub(crate) use sentinel_extract::render_sentinel_blocks;
pub use static_file::render_static_file;

#[cfg(test)]
pub(super) mod test_helpers {
    use crate::core::{FieldDefinition, FieldType, LocalizedString, SelectOption};

    pub fn text_field(name: &str, required: bool) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Text)
            .required(required)
            .build()
    }

    pub fn select_field(name: &str, required: bool, opts: &[&str]) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Select)
            .required(required)
            .options(
                opts.iter()
                    .map(|v| SelectOption::new(LocalizedString::Plain(v.to_string()), *v))
                    .collect(),
            )
            .build()
    }

    pub fn checkbox_field(name: &str) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Checkbox).build()
    }
}
