//! Filter clause building (SQL WHERE) and in-memory evaluation.

mod decode;
mod elements;
pub mod memory;
mod operators;
mod resolve;
mod subquery;
mod where_clause;

pub use decode::{decode_where_json_str, decode_where_map};
#[cfg(all(test, feature = "postgres"))]
pub(crate) use operators::build_op_condition;
pub use resolve::normalize_filter_fields;
pub(crate) use resolve::{lookup_column_field, lookup_column_field_type};
pub use where_clause::build_where_clause;
