//! Crap CMS library — headless CMS with Lua hooks, gRPC API, and an HTMX admin UI.

// Alias the current crate as `crap_cms` so proc-macros in `crap-cms-macros`
// can emit fully-qualified `::crap_cms::typegen::lua::*` paths and have
// them resolve both from inside this crate (where `crate::` would normally
// be used) and from external test fixtures. Standard pattern for
// proc-macro-emitted absolute paths.
extern crate self as crap_cms;

pub mod admin;
pub mod api;
pub mod cli;
pub mod commands;
pub mod config;
pub mod core;
pub mod db;
pub mod fmt;
pub mod hooks;
pub mod mcp;
pub mod scaffold;
pub mod scheduler;
pub mod service;
pub mod typegen;

/// Generators that produce documentation artefacts from typed Rust
/// source. Sibling namespace to [`typegen`] for items that aren't
/// type definitions per se — e.g. the admin-template-context reference.
/// Consumed by `cargo xtask gen-*` subcommands.
pub mod docgen {
    pub use crate::admin::context::page::schema_doc::generate_template_context_md;
}
