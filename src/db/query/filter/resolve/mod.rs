//! Dot notation normalization and filter path resolution.
//!
//! Converts dot-notation filter fields to their SQL representations:
//! - Group fields (`seo.meta_title`) → flat columns (`seo__meta_title`)
//! - Array/Blocks/Relationship sub-fields → EXISTS subquery descriptors

mod json_walk;
mod lookup;
mod normalize;
mod path;
mod types;

#[cfg(test)]
mod test_helpers;

pub(super) use lookup::typed_system_columns;
pub(crate) use lookup::{lookup_column_field, lookup_column_field_type};
pub use normalize::normalize_filter_fields;
pub(super) use path::{ROW_ID, resolve_filter};
pub(super) use types::{ResolvedFilter, RowsLocale, SubqueryCondition};
