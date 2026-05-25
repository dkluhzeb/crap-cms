//! Filter clause building (SQL WHERE) and in-memory evaluation.

pub mod memory;
mod operators;
mod resolve;
mod where_clause;

pub use resolve::normalize_filter_fields;
pub(crate) use where_clause::resolve_filter_column;
pub use where_clause::{build_where_clause, resolve_filters};
