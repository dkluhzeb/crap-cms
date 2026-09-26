//! Reference counting for delete protection.
//!
//! Tracks how many documents reference a given target via `_ref_count` columns.
//! Replaces the O(N) back-reference scan with O(1) delete-protection checks.

mod added;
mod anchor;
mod change;
mod compute;
mod count;
mod create;
mod delta;
mod outgoing_ref;
mod read;
mod touches;
mod walk;

#[cfg(test)]
mod test_helpers;

pub use added::AddedReferences;
pub use anchor::anchor_to_fields;
pub use change::{after_import, after_update, before_hard_delete, snapshot_outgoing_refs};
pub use count::{get_purgeable_ref_count_locked, get_ref_count, get_ref_count_locked};
pub use create::{after_create, after_create_from_data, backfill_after_create};
pub use delta::UnavailableReferences;
pub use outgoing_ref::OutgoingRef;
pub use touches::data_touches_refs;
pub(crate) use walk::{walk_blocks_with, walk_nested_with};
