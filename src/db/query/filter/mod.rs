//! Filter clause building (SQL WHERE) and in-memory evaluation.

mod day;
mod decode;
mod elements;
mod error;
#[cfg(test)]
pub(crate) mod localized_rows_fixture;
pub mod memory;
mod operators;
mod resolve;
#[cfg(test)]
pub(crate) mod row_paths_fixture;
mod subquery;
mod where_clause;

pub use decode::{decode_where_json_str, decode_where_map};
pub(crate) use error::invalid_query;
#[cfg(all(test, feature = "postgres"))]
pub(crate) use operators::build_op_condition;
pub use resolve::normalize_filter_fields;
pub(crate) use resolve::{lookup_column_field, lookup_column_field_type};
pub use where_clause::build_where_clause;
