//! Version-specific database operations for the `_versions_{slug}` table.

mod crud;
mod restore;
mod snapshot;

pub use crud::{
    VersionWrite, count_versions, create_version, create_version_and_prune, document_is_live,
    find_latest_published_version, find_latest_version, find_version_by_id, get_document_status,
    list_snapshots, list_versions, prune_versions, set_document_status,
};

pub use snapshot::build_snapshot;
pub(crate) use snapshot::{JoinOwner, locale_join_rows, localized_join_keys};

pub(crate) use restore::{LocaleSnapshot, SnapshotKey};
pub use restore::{
    restore_global_version, restore_version, snapshot_write_fields, write_global_snapshot_base,
    write_snapshot_base,
};
