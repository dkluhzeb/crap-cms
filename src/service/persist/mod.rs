//! DB write phase functions for collection CRUD operations.
//!
//! Each `persist_*` function handles the database-level work for a single operation:
//! insert/update rows, join table data, passwords, and version snapshots.

mod create;
mod email_change;
mod search_index;
mod update;
mod version;

pub use create::persist_create;
pub(crate) use search_index::sync_search_index;
pub(crate) use update::persist_bulk_update;
pub use update::persist_update;
pub use version::{DraftDocumentArgs, draft_document, persist_draft_version, persist_unpublish};
