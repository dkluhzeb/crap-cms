//! Global document update orchestration.

mod unpublish;
mod update;

pub use unpublish::unpublish_global_document;
pub(crate) use update::{check_global_update_access, stored_global_fields_for_update_rules};
pub use update::{update_global_document, update_global_in_conn};
