//! Global document update orchestration.

mod unpublish;
mod update;

pub use unpublish::unpublish_global_document;
pub use update::{update_global_core, update_global_document};
