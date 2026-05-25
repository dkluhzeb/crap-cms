//! Dot notation normalization and filter path resolution.
//!
//! Converts dot-notation filter fields to their SQL representations:
//! - Group fields (`seo.meta_title`) → flat columns (`seo__meta_title`)
//! - Array/Blocks/Relationship sub-fields → EXISTS subquery descriptors

mod blocks;
mod lookup;
mod normalize;
mod path;
mod types;

#[cfg(test)]
mod test_helpers;

pub use normalize::normalize_filter_fields;
pub(super) use path::resolve_filter;
pub(super) use types::{ResolvedFilter, SubqueryCondition};
