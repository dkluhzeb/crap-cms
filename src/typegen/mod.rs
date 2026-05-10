//! Type generation for multiple languages from the collection registry.
//!
//! - `lua` — LuaLS annotations for hook/init IDE support (internal,
//!   default backend used on server startup)
//! - `typescript` — TypeScript interfaces for gRPC clients
//! - `go` — Go structs with json tags
//! - `python` — Python dataclasses
//! - `rust_types` — Rust structs with serde derives
//! - `rust_proto` — Rust prost_types → typed-struct conversion impls
//!
//! ## Layout
//!
//! - [`Language`] (enum + accessors) — `language.rs`
//! - File-output entry points (`generate`, `generate_lang`,
//!   `generate_proto_conversion`) + the per-language render dispatch —
//!   `dispatch.rs`
//! - Cross-language helpers (`to_pascal_case`, `is_optional`,
//!   `rel_has_many`, `sorted_*_slugs`, `collect_sub_type_fields`) —
//!   `helpers.rs`
//! - Per-language render backends — `lua.rs`, `typescript.rs`, etc.
//!
//! ## Cross-module re-export
//!
//! [`to_pascal_case`] is `pub(crate)` and reached from
//! `scaffold::{job,hook}::generator` via `crate::typegen::to_pascal_case`
//! — re-exported at the module root for short-path access.

mod dispatch;
mod go;
mod helpers;
mod language;
mod lua;
mod python;
mod rust_proto;
mod rust_types;
mod typescript;

pub use dispatch::{generate, generate_lang, generate_proto_conversion};
pub(crate) use helpers::to_pascal_case;
pub use language::Language;
