//! Version-specific database operations for the `_versions_{slug}` table.

mod crud;
mod restore;
mod snapshot;

pub use crud::{
    count_versions, create_version, find_latest_version, find_version_by_id, get_document_status,
    list_versions, prune_versions, set_document_status,
};

pub use snapshot::build_snapshot;

pub use restore::{restore_global_version, restore_version};
