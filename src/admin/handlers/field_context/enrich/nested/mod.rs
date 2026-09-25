//! Builds enriched sub-field contexts for array/blocks rows and recursively
//! enriches nested relationship/upload fields with DB-fetched options.

mod dispatch;
mod selection;
mod sub_field;

pub use selection::enrich_nested_fields;
pub use sub_field::build_enriched_sub_field_context;

pub(in crate::admin::handlers::field_context::enrich) use dispatch::{
    construct_sub_variant, enrich_sub_richtext,
};
