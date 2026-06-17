//! Version management: snapshots, drafts, restore, list, unpublish.

mod find;
mod gate;
mod list;
mod restore;
mod save_draft;
pub(crate) mod snapshot;
mod unpublish;

pub use find::find_version_by_id;
pub(crate) use save_draft::{SaveDraftArgs, save_draft_version};
pub(crate) use snapshot::{VersionSnapshotCtx, create_version_snapshot};
pub(crate) use unpublish::unpublish_with_snapshot;

pub use list::list_versions;
#[cfg(all(test, feature = "sqlite"))]
pub(crate) use restore::restore_collection_version_core;
pub use restore::{restore_collection_version, restore_global_version};
