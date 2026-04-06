//! Write operations: create, update, delete.

mod create;
mod delete;
mod update;

pub use create::create;
pub use delete::{delete, restore, soft_delete};
pub(in crate::db::query) use update::{UpdateCollector, collect_update_params};
pub use update::{update, update_partial};
