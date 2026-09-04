//! Handlebars template loading with overlay (config dir overrides compiled defaults).

mod helpers;
mod registry;
pub(crate) mod render_scope;
pub mod slot_docs;

pub use registry::create_handlebars;
