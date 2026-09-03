//! Generators that produce documentation artefacts from typed Rust
//! source. Sibling namespace to [`typegen`] for items that aren't
//! type definitions per se — e.g. the admin-template-context reference.
//! Consumed by `cargo xtask gen-*` subcommands.

pub mod components;
pub mod css_tokens;
pub mod mcp_reserved;
pub mod region;

pub use components::generate_component_table;

pub use mcp_reserved::generate_mcp_reserved_args_table;

pub use css_tokens::generate_css_variables_md;

pub use crate::admin::context::page::schema_doc::generate_template_context_md;
pub use crate::admin::templates::slot_docs::generate_slots_table;
pub use crate::service::op::wire_doc::generate_wire_reference_md;
