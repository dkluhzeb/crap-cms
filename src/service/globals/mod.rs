//! Global document update orchestration.

mod unpublish;
mod update;

pub use unpublish::unpublish_global_document;
pub(crate) use update::check_global_update_access;
pub use update::{update_global_document, update_global_in_conn};
