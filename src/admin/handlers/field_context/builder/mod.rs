//! Build field context objects for template rendering (no DB access).

mod context;
mod single;

pub use context::build_field_contexts;
pub(super) use context::build_select_options;
pub use single::build_single_field_context;
