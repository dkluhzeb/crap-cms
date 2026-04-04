//! Shared helpers for migration: table introspection, column specs, join tables, versions.

mod column_specs;
mod introspection;
mod join_tables;
mod versions;

pub(super) use column_specs::{ColumnSpec, collect_column_specs};
pub use introspection::{get_table_column_types, get_table_columns, sanitize_locale, table_exists};
pub(super) use join_tables::sync_join_tables;
pub(super) use versions::sync_versions_table;
