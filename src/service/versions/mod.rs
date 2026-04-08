//! Version management: snapshots, drafts, restore, list, unpublish.

mod list;
mod restore;
mod save_draft;
pub(crate) mod snapshot;
mod unpublish;

pub(crate) use save_draft::save_draft_version;
pub(crate) use snapshot::{VersionSnapshotCtx, create_version_snapshot};
pub use unpublish::unpublish_with_snapshot;

pub use list::list_versions;
pub use restore::{restore_collection_version, restore_global_version};
