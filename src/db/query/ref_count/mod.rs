//! Reference counting for delete protection.
//!
//! Tracks how many documents reference a given target via `_ref_count` columns.
//! Replaces the O(N) back-reference scan with O(1) delete-protection checks.

mod api;
mod compute;
mod delta;
mod outgoing_ref;
mod read;
mod walk;

#[cfg(test)]
mod test_helpers;

pub use api::{
    after_create, after_create_from_data, after_update, before_hard_delete, data_touches_refs,
    get_ref_count, get_ref_count_locked, lock_ref_targets_from_data, snapshot_outgoing_refs,
};
pub use outgoing_ref::OutgoingRef;
pub(crate) use walk::{RefPathSeg, walk_blocks_with, walk_nested_with};
