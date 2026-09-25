//! Select/radio options for the form: the declared ones, plus a stored value
//! the field no longer declares.

use std::collections::HashSet;

use crate::{
    admin::{context::field::SelectOption, handlers::field_context::tag_values},
    core::FieldDefinition,
};

/// The values a Select/Radio field holds: the whole list for `has_many`, the
/// single stored value otherwise.
fn selected_values(field: &FieldDefinition, value: &str) -> Vec<String> {
    if field.has_many {
        tag_values(value)
    } else {
        vec![value.to_string()]
    }
}

/// Options for values the field no longer declares, marked `unlisted`.
///
/// A stored value outside `options` used to render as nothing: the picker came
/// back blank and re-saving the form dropped the value. Keeping it as a
/// selected option shows the editor what the document holds and survives the
/// round trip.
fn unlisted_options(field: &FieldDefinition, selected: &[String]) -> Vec<SelectOption> {
    let mut seen = HashSet::new();

    selected
        .iter()
        .filter(|value| !value.is_empty())
        .filter(|value| !field.options.iter().any(|opt| opt.value == **value))
        .filter(|value| seen.insert((*value).clone()))
        .map(|value| SelectOption {
            label: value.clone(),
            value: value.clone(),
            selected: true,
            unlisted: true,
        })
        .collect()
}

/// Build select/radio options with `selected` flags, handling both single and
/// multi-select. Returns `(options, is_has_many)`.
pub(in crate::admin::handlers::field_context) fn build_select_options(
    field: &FieldDefinition,
    value: &str,
) -> (Vec<SelectOption>, bool) {
    let selected = selected_values(field, value);

    let mut options: Vec<SelectOption> = field
        .options
        .iter()
        .map(|opt| SelectOption {
            label: opt.label.resolve_current().to_string(),
            value: opt.value.clone(),
            selected: selected.contains(&opt.value),
            unlisted: false,
        })
        .collect();

    options.extend(unlisted_options(field, &selected));

    (options, field.has_many)
}

#[cfg(test)]
mod tests {
    use crate::{
        admin::handlers::field_context::test_helpers::make_field,
        core::{FieldType, LocalizedString, SelectOption as CoreSelectOption},
    };

    use super::*;

    // ── Select options ────────────────────────────────────────────────

    fn choice(name: &str, has_many: bool, values: &[&str]) -> FieldDefinition {
        let mut field = make_field(name, FieldType::Select);
        field.has_many = has_many;
        field.options = values
            .iter()
            .map(|v| CoreSelectOption::new(LocalizedString::Plain(v.to_uppercase()), *v))
            .collect();

        field
    }

    /// Regression: a stored value the field no longer declares rendered as
    /// nothing — the picker came back blank and re-saving the form dropped the
    /// value. It is kept as a selected option, marked `unlisted`.
    #[test]
    fn a_stored_value_outside_the_options_is_kept_and_marked() {
        let field = choice("status", false, &["draft"]);
        let (options, has_many) = build_select_options(&field, "gone");

        assert!(!has_many);
        assert_eq!(options.len(), 2);
        assert_eq!(
            (
                options[0].value.as_str(),
                options[0].selected,
                options[0].unlisted
            ),
            ("draft", false, false)
        );
        assert_eq!(
            (
                options[1].value.as_str(),
                options[1].label.as_str(),
                options[1].selected,
                options[1].unlisted
            ),
            ("gone", "gone", true, true),
            "the submitted value survives the re-render, marked as undeclared"
        );
    }

    /// The same for a `has_many` picker, and a repeated undeclared value is
    /// kept once. An empty value never becomes an option — that is the
    /// template's placeholder.
    #[test]
    fn unlisted_values_are_deduped_and_never_empty() {
        let field = choice("tags", true, &["a"]);
        let (options, has_many) = build_select_options(&field, r#"["a","x","x",""]"#);

        assert!(has_many);
        let unlisted: Vec<&str> = options
            .iter()
            .filter(|o| o.unlisted)
            .map(|o| o.value.as_str())
            .collect();
        assert_eq!(unlisted, vec!["x"]);
        assert!(options[0].selected, "a declared stored value is selected");
    }

    /// A declared option list with nothing stored is untouched.
    #[test]
    fn declared_options_are_unchanged_without_a_stored_value() {
        let field = choice("status", false, &["draft", "live"]);
        let (options, _) = build_select_options(&field, "");

        assert_eq!(options.len(), 2);
        assert!(options.iter().all(|o| !o.unlisted && !o.selected));
    }
}
