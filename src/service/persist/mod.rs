//! DB write phase functions for collection CRUD operations.
//!
//! Each `persist_*` function handles the database-level work for a single operation:
//! insert/update rows, join table data, passwords, and version snapshots.

mod create;
mod update;
mod version;

pub use create::persist_create;
pub use update::{persist_bulk_update, persist_update};
pub use version::{persist_draft_version, persist_unpublish};
