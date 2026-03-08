//! Version-specific database operations for the `_versions_{slug}` table.

mod crud;
mod snapshot;
mod restore;

pub use crud::{
    create_version,
    find_latest_version,
    count_versions,
    list_versions,
    find_version_by_id,
    prune_versions,
    set_document_status,
    get_document_status,
};

pub use snapshot::build_snapshot;

pub use restore::{restore_version, restore_global_version};
