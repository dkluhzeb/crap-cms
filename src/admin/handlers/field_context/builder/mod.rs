//! Build field context objects for template rendering (no DB access).

mod context;
mod options;
mod single;

pub use context::build_field_contexts;
pub(in crate::admin::handlers::field_context) use context::visible_field_defs;
pub(super) use options::build_select_options;
pub use single::build_single_field_context;
