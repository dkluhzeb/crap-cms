//! Proc-macro crate for crap-cms typegen + Lua API binding.
//!
//! Hosts the derives and macros that drive `types/crap.lua` generation
//! from Rust source. Trait targets (`LuaAnnotation`, `LuaAlias`,
//! `LuaTypeAlias`, `LuaFieldTypeViews`, `LuaFieldBlock`,
//! `LuaFieldTypeViewsDiscriminator`) live in
//! `crap_cms::typegen::lua::annotation` — proc-macro crates can only
//! export proc-macros, so the traits stay in the main crate and the
//! derive output references them via `crate::typegen::lua::*` (resolved
//! via the main crate's `extern crate self as crap_cms`).
//!
//! ## Layout
//!
//! Each macro lives in its own module; this file only registers them
//! at the crate root (a Rust proc-macro requirement) and forwards to
//! the module implementations.
//!
//! - [`shared`] — cross-derive helpers: doc extraction, type mapping,
//!   field-line emission, rename-strategy application.
//! - [`lua_annotation`] — `#[derive(LuaAnnotation)]`.
//! - [`lua_alias`] — `#[derive(LuaAlias)]` (literal- or type-union enums).
//! - [`lua_type_alias`] — `#[derive(LuaTypeAlias)]` (callable / literal
//!   target aliases on unit structs).
//! - [`lua_field_type_views`] — `#[derive(LuaFieldTypeViews)]`
//!   (discriminated catch-all → base + per-variant subclasses).
//! - [`lua_fn`] — `#[lua_fn(...)]` attribute macro.
//! - [`lua_table`] — `lua_table! { ... }` function-like macro.
//!
//! ## Auto-mapped Rust → Lua types
//!
//! | Rust                                            | Lua          |
//! |-------------------------------------------------|--------------|
//! | `String`, `&str`, `&'static str`                | `string`     |
//! | `bool`                                          | `boolean`    |
//! | `u8`/`u16`/.../`usize`, `i8`/.../`isize`        | `integer`    |
//! | `f32`, `f64`                                    | `number`     |
//! | `Option<T>`                                     | `T?`         |
//! | `Vec<String>` (or other scalar)                 | `string[]`   |
//! | `Vec<T: LuaAnnotation>`                         | `T[]` (resolved via `<T as LuaAnnotation>::CLASS_NAME`) |
//! | Bare path `T`                                   | `T::CLASS_NAME` (resolved at runtime — must impl `LuaAnnotation`) |
//! | `HashMap<K, V>` (in `#[lua_fn]` types only)     | `table<K, V>` |
//!
//! Unrecognized types emit a compile error and require an explicit
//! `#[lua(ty = "...")]` override.

// `clippy::needless_continue` fires inside `darling`'s `FromDeriveInput`
// / `FromField` expansion (their synthesized parser uses `continue` in
// a way clippy doesn't like). The lint surfaces at the derive name in
// our source, so a struct-level `#[allow]` doesn't reach it. Crate-wide
// is fine — this proc-macro crate has no hand-written `continue` of
// its own.
#![allow(clippy::needless_continue)]

mod lua_alias;
mod lua_annotation;
mod lua_field_type_views;
mod lua_fn;
mod lua_table;
mod lua_tagged_class;
mod lua_type_alias;
mod shared;

use proc_macro::TokenStream;

/// See [`lua_annotation`] for full attribute reference.
#[proc_macro_derive(LuaAnnotation, attributes(lua))]
pub fn derive_lua_annotation(input: TokenStream) -> TokenStream {
    lua_annotation::derive(input)
}

/// See [`lua_alias`] for full attribute reference.
#[proc_macro_derive(LuaAlias, attributes(lua))]
pub fn derive_lua_alias(input: TokenStream) -> TokenStream {
    lua_alias::derive(input)
}

/// See [`lua_type_alias`] for full attribute reference.
#[proc_macro_derive(LuaTypeAlias, attributes(lua))]
pub fn derive_lua_type_alias(input: TokenStream) -> TokenStream {
    lua_type_alias::derive(input)
}

/// See [`lua_tagged_class`] for full attribute reference.
#[proc_macro_derive(LuaTaggedClass, attributes(lua))]
pub fn derive_lua_tagged_class(input: TokenStream) -> TokenStream {
    lua_tagged_class::derive(input)
}

/// See [`lua_field_type_views`] for full attribute reference.
#[proc_macro_derive(LuaFieldTypeViews, attributes(lua))]
pub fn derive_lua_field_type_views(input: TokenStream) -> TokenStream {
    lua_field_type_views::derive(input)
}

/// See [`lua_fn`] for full attribute reference.
#[proc_macro_attribute]
pub fn lua_fn(attr: TokenStream, item: TokenStream) -> TokenStream {
    lua_fn::run(attr, item)
}

/// See [`lua_table`] for full syntax reference.
#[proc_macro]
pub fn lua_table(input: TokenStream) -> TokenStream {
    lua_table::run(input)
}
