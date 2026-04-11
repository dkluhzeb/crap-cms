//! Top-level field context building — select options and field list assembly.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, from_str, json};

use crate::core::FieldDefinition;

use super::single::build_single_field_context;

/// Build select/radio options with `selected` flags, handling both single and multi-select.
/// Returns `(options_json, is_has_many)`.
pub(in crate::admin::handlers::field_context) fn build_select_options(
    field: &FieldDefinition,
    value: &str,
) -> (Vec<Value>, bool) {
    if field.has_many {
        let selected_values: HashSet<String> = from_str(value).unwrap_or_default();

        let options: Vec<_> = field
            .options
            .iter()
            .map(|opt| {
                json!({
                    "label": opt.label.resolve_default(),
                    "value": opt.value,
                    "selected": selected_values.contains(&opt.value),
                })
            })
            .collect();

        (options, true)
    } else {
        let options: Vec<_> = field
            .options
            .iter()
            .map(|opt| {
                json!({
                    "label": opt.label.resolve_default(),
                    "value": opt.value,
                    "selected": opt.value == value,
                })
            })
            .collect();

        (options, false)
    }
}

/// Build field context objects for template rendering.
///
/// `non_default_locale`: when true, non-localized fields are rendered readonly
/// (locked) because they are shared across all locales and should only be edited
/// from the default locale.
pub fn build_field_contexts(
    fields: &[FieldDefinition],
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    filter_hidden: bool,
    non_default_locale: bool,
) -> Vec<Value> {
    let iter: Box<dyn Iterator<Item = &FieldDefinition>> = if filter_hidden {
        Box::new(fields.iter().filter(|field| !field.admin.hidden))
    } else {
        Box::new(fields.iter())
    };

    iter.map(|field| build_single_field_context(field, values, errors, "", non_default_locale, 0))
        .collect()
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
