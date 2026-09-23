//! Pure list-view helpers — columns, cells, filter pills, column picker.
//!
//! These are data-transformation functions for the collection list page.
//! No async, no DB, no HTTP — all take definitions + documents and return JSON.

mod access;
mod cells;
mod columns;
mod filters;
#[cfg(test)]
mod test_helpers;

pub(super) use access::ListFieldAccess;
pub(super) use cells::compute_cells;
pub(super) use columns::{build_column_options, resolve_columns, title_label};
pub(super) use filters::{
    FilterPillInputs, active_filter_count, build_filter_fields, build_filter_pills,
};
