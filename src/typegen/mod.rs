//! Type generation for multiple languages from the collection registry.
//!
//! - `lua` — `LuaLS` annotations for hook/init IDE support (internal,
//!   default backend used on server startup)
//! - `client` — the four client-SDK type-definition generators (TypeScript,
//!   Go, Python, Rust), built on a shared schema walk feeding a per-language
//!   `ClientPrinter` (see `client/mod.rs`)
//! - `rust_proto` — typed proto (`FieldValue`/`DataMap`) → typed-struct conversion impls
//!
//! ## Layout
//!
//! - [`Language`] (client-language enum + accessors) — `language.rs`
//! - File-output entry points (`generate_lua`, `generate_client`,
//!   `generate_proto`) + the per-language render dispatch —
//!   `dispatch.rs`
//! - Cross-language helpers (`to_pascal_case`, `is_optional`,
//!   `rel_has_many`, `sorted_*_slugs`, `collect_sub_type_fields`) —
//!   `helpers.rs`
//! - Client-SDK backends — `client/` (`rust.rs`, `typescript.rs`,
//!   `go.rs`, `python.rs`, driven by `client/mod.rs`); Lua — `lua/`;
//!   Rust proto conversion — `rust_proto.rs`.
//!
//! ## Cross-module re-export
//!
//! [`to_pascal_case`] is `pub(crate)` and reached from
//! `scaffold::{job,hook}::generator` via `crate::typegen::to_pascal_case`
//! — re-exported at the module root for short-path access.

mod client;
mod dispatch;
mod helpers;
mod idents;
mod language;
// `lua` is `pub` (not `mod`) so xtask + proc-macro-emitted paths can
// reach `crap_cms::typegen::lua::{ensure_table, format_lua_fn_spec,
// render_static_file, …}`. The submodule's internal items remain
// gated; only what's `pub use`'d at its mod root is reachable.
pub mod lua;
mod rust_proto;

pub use dispatch::{generate_client, generate_lua, generate_proto};
pub(crate) use helpers::to_pascal_case;
pub use language::Language;
pub use lua::LuaAnnotation;
