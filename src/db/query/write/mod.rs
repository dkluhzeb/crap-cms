//! Write operations: create, update, delete.

mod create;
mod delete;
mod revision;
mod stale_locale_rows;
mod update;

pub use create::create;
pub use delete::{delete, restore, soft_delete};
pub use revision::{REVISION_COLUMN, RevisionAdvance, advance_revision, read_revision};
pub use stale_locale_rows::delete_rows_outside_locales;
pub use update::{DocumentNotFound, update, update_partial};
pub(in crate::db::query) use update::{UpdateCollector, collect_update_params};
