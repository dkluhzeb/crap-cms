//! Conversion helpers: document/field/value conversions between Rust types and protobuf.

mod document;
mod filters;
mod pagination;
mod schema;

pub(in crate::api::handlers) use document::{
    data_map_to_json_map, document_to_proto, json_to_field_value,
};
pub use filters::parse_where_json;
pub(in crate::api::handlers) use pagination::{
    clamp_limit, floor_optional_limit, pagination_result_to_proto,
};
pub(in crate::api::handlers) use schema::field_def_to_proto;
