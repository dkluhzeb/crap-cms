//! Top-level field context building — select options and field list assembly.

use std::collections::{HashMap, HashSet};

use serde_json::from_str;

use crate::{
    admin::context::field::{FieldContext, SelectOption},
    core::FieldDefinition,
};

use super::single::build_single_field_context;

/// Build select/radio options with `selected` flags, handling both single and
/// multi-select. Returns `(options, is_has_many)`.
pub(in crate::admin::handlers::field_context) fn build_select_options(
    field: &FieldDefinition,
    value: &str,
) -> (Vec<SelectOption>, bool) {
    if field.has_many {
        let selected_values: HashSet<String> = from_str(value).unwrap_or_default();

        let options: Vec<_> = field
            .options
            .iter()
            .map(|opt| SelectOption {
                label: opt.label.resolve_default().to_string(),
                value: opt.value.clone(),
                selected: selected_values.contains(&opt.value),
            })
            .collect();

        (options, true)
    } else {
        let options: Vec<_> = field
            .options
            .iter()
            .map(|opt| SelectOption {
                label: opt.label.resolve_default().to_string(),
                value: opt.value.clone(),
                selected: opt.value == value,
            })
            .collect();

        (options, false)
    }
}

/// Build field context objects for template rendering.
///
/// `filter_hidden`: when true, fields with `admin.hidden = true` are skipped
/// (form rendering — false during error re-renders so the user's entered
/// values are preserved across the round-trip).
///
/// Fields with top-level `hidden = true` are *always* skipped — the data is
/// stripped from API responses, so there's nothing to render and no value to
/// preserve. The `filter_hidden` toggle does not apply.
///
/// `non_default_locale`: when true, non-localized fields are rendered readonly
/// (locked) because they are shared across all locales and should only be
/// edited from the default locale.
///
/// Returns `Vec<FieldContext>` — typed end-to-end. Downstream consumers
/// (`enrich_field_contexts`, `apply_display_conditions`,
/// `split_sidebar_fields`, the `fields` field on each typed page context)
/// all consume typed values; serialization to `serde_json::Value` happens
/// once when the page context is serialized for the `before_render` hook.
pub fn build_field_contexts(
    fields: &[FieldDefinition],
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    filter_hidden: bool,
    non_default_locale: bool,
) -> Vec<FieldContext> {
    fields
        .iter()
        .filter(|field| !field.hidden)
        .filter(|field| !filter_hidden || !field.admin.hidden)
        .map(|field| build_single_field_context(field, values, errors, "", non_default_locale, 0))
        .collect()
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
