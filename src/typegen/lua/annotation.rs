//! Per-type trait for emitting `lua-language-server` class annotations.
//!
//! Each Rust struct that backs a Handlebars template-context shape
//! implements [`LuaAnnotation`] *next to its definition* — so the
//! annotation lives one screen away from the field list it mirrors.
//! The aggregate render fn ([`render::render_template_data_types`]) is
//! then a clean dispatch over the types in document order.
//!
//! Drift between the Rust struct and the annotation is still
//! hand-maintained; a `#[derive(LuaAnnotation)]` proc-macro would
//! eliminate that and is the eventual target.
//!
//! [`render::render_template_data_types`]: super::render

/// A Rust type that has a corresponding `lua-language-server` class
/// annotation. Implementors emit a `---@class <name>` block + one
/// `---@field <name> <type>` line per public field, followed by a blank
/// line so the next annotation reads as its own paragraph.
pub trait LuaAnnotation {
    /// Append this type's `---@class` block to `out`.
    fn render_lua_annotation(out: &mut String);
}
