//! Shared helpers for migration: table introspection, column specs, join
//! tables, versions, and — for the one-time conversions — the gate value they
//! store and the paged reader they scan with.

mod alter;
mod column_specs;
mod gate;
mod introspection;
mod join_tables;
mod paged;
mod versions;

pub(in crate::db::migrate) use alter::{
    add_column_if_missing, check_type_mismatch, reconcile_scalar_list_column, warn_orphan_columns,
};
pub(super) use column_specs::{ColumnSpec, collect_column_specs};
pub(super) use gate::{block_paths, field_paths, versioned_fingerprint};
pub use introspection::sanitize_locale;
pub(crate) use introspection::{get_table_column_types, get_table_columns, table_exists};
pub(super) use join_tables::sync_join_tables;
pub(super) use paged::{Scan, for_each_row, update_by_id};
pub(super) use versions::sync_versions_table;

/// The page size a conversion's own test fills past, to prove its scan reaches
/// the rows beyond the first page.
#[cfg(test)]
pub(super) use paged::PAGE_SIZE;
