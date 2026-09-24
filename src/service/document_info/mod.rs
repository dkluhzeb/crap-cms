//! Document information service — ref counts, back-references, missing relations.
//!
//! Thin service wrappers for consistency. All future surfaces should call these
//! instead of the query layer directly.

mod back_references;
mod version_gaps;

#[cfg(all(test, feature = "sqlite"))]
mod test_support;

pub use back_references::{BackReferenceReport, find_back_references, get_ref_count};
pub use version_gaps::{VersionGaps, version_restore_gaps};
