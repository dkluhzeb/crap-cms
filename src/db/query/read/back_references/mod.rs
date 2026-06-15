//! Scan all collections and globals for documents that reference a given target document.
//! Also: check version snapshots for missing (deleted) relationship targets.

mod helpers;
mod scan;
mod sub_fields;
mod types;
mod visibility;

#[cfg(test)]
mod test_helpers;

pub(super) use helpers::field_display_label;
pub use scan::find_back_references;
pub use types::BackReference;
pub use visibility::filter_visible_ids;
