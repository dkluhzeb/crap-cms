//! Handlebars template loading with overlay (config dir overrides compiled defaults).

mod helpers;
mod registry;
pub mod slot_docs;

pub use registry::create_handlebars;
