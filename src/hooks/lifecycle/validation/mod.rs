//! Field validation: required checks, unique checks, date format, custom Lua
//! validators, and richtext node-attr validation.

mod checks;
mod completeness;
mod context;
mod custom;
mod recursive;
pub(crate) mod richtext_attrs;
mod runner;
mod sub_fields;

pub use checks::is_valid_email_format;
pub(in crate::hooks::lifecycle::validation) use completeness::check_localized_completeness;
pub use context::ValidationCtx;
pub(in crate::hooks::lifecycle::validation) use runner::is_empty_value;
pub(crate) use runner::validate_fields_inner;
