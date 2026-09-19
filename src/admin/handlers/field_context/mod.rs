//! Field context builders for admin form rendering.
//! Builds template context objects from field definitions, handling recursive
//! composite types (Array, Blocks, Group) with nesting depth limits.

mod builder;
mod enrich;
mod helpers;

#[cfg(test)]
mod test_helpers;

pub(super) use builder::build_field_contexts;
pub(super) use enrich::{EnrichOptions, enrich_field_contexts};
pub(super) use helpers::{
    MAX_FIELD_DEPTH, apply_display_conditions, collect_node_attr_errors,
    count_errors_in_field_contexts, date_picker_values, inject_lang_values_from_row,
    inject_timezone_values_from_row, json_textarea_value, locale_locked_display,
    localize_date_display, picker_step, safe_template_id, set_date_picker_values,
    split_sidebar_fields, tag_values, tag_values_of, tags_input_value,
};
