//! Handlebars template loading with overlay (config dir overrides compiled defaults).

mod helpers;
mod registry;

pub use registry::create_handlebars;
