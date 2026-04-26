//! Field context builders for admin form rendering.
//! Builds template context objects from field definitions, handling recursive
//! composite types (Array, Blocks, Group) with nesting depth limits.

mod builder;
mod enrich;
mod helpers;

pub(super) use builder::build_field_contexts;
pub(super) use enrich::{EnrichOptions, enrich_field_contexts};
pub(super) use helpers::{
    MAX_FIELD_DEPTH, add_timezone_context, apply_display_conditions, collect_node_attr_errors,
    count_errors_in_fields, inject_lang_values_from_row, inject_timezone_values_from_row,
    safe_template_id, split_sidebar_fields,
};
