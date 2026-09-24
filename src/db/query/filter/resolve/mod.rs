//! Dot notation normalization and filter path resolution.
//!
//! Converts dot-notation filter fields to their SQL representations:
//! - Group fields (`seo.meta_title`) → flat columns (`seo__meta_title`)
//! - Array/Blocks/has-many sub-fields — at the top level or inside groups
//!   (`seo.items.name`) → EXISTS subquery descriptors

mod container;
mod json_walk;
mod lookup;
mod normalize;
mod path;
mod rows;
mod types;

#[cfg(test)]
mod test_helpers;

pub(super) use container::container_root;
pub(super) use lookup::typed_system_columns;
pub(crate) use lookup::{lookup_column_field, lookup_column_field_type};
pub use normalize::{normalize_filter_fields, normalize_order_by};
pub(super) use path::resolve_filter;
pub(super) use rows::ROW_ID;
pub(super) use types::{JsonLeaf, JsonStep, ResolvedFilter, RowsLocale, SubqueryCondition};
