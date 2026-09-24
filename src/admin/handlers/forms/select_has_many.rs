//! Normalize `has_many` select/radio/text/number form values into JSON array strings.
//!
//! Two input shapes are accepted:
//! - JSON array (`"[\"a\",\"b\"]"`) — what `parse_form` makes of a name the
//!   form submitted more than once (`<select multiple>`), what the tag widget
//!   writes, and what the `<crap-validate-form>` JSON endpoint forwards
//!   (`values_to_string_map` serializes array values with `Value::to_string()`).
//! - Any other text — exactly one value (a `<select multiple>` with a single
//!   option picked). It is never split on commas, so an option value that
//!   contains a comma survives.

use serde_json::{Value, json};
use std::collections::HashMap;

use crate::{
    admin::handlers::shared::for_each_admin_form_leaf,
    core::{FieldDefinition, FieldType},
};

/// Normalize `has_many` select/radio/text/number form values into canonical JSON
/// array strings. The flat-column walk ([`for_each_admin_form_leaf`]) handles
/// Group `__`-prefixing and transparent layout wrappers.
///
/// Only the leaves the form rendered are normalized: an absent one means the
/// editor cleared the list, which is an empty list. A `has_many` the form never
/// rendered (`admin.hidden`, or one inside a hidden group) is left absent so its
/// stored list survives the save — the same rule the checkbox normalizer
/// follows, from the same answer.
pub(crate) fn transform_select_has_many(
    form: &mut HashMap<String, String>,
    field_defs: &[FieldDefinition],
) {
    // Collect transforms first to avoid double-borrow on `form`
    let mut updates: Vec<(String, String)> = Vec::new();

    for_each_admin_form_leaf(field_defs, |field, column| {
        let is_multi_leaf = matches!(
            field.field_type,
            FieldType::Select | FieldType::Radio | FieldType::Text | FieldType::Number
        );
        if !is_multi_leaf || !field.has_many {
            return;
        }

        let json_val = form
            .get(&column)
            .map_or_else(|| "[]".to_string(), |val| canonical_json_array(val));
        updates.push((column, json_val));
    });

    for (name, val) in updates {
        form.insert(name, val);
    }
}

/// Normalize one raw form value into a canonical JSON string array.
///
/// Shared with the composite parser, which normalizes `has_many` sub-fields
/// nested inside array/blocks rows (which this module's top-level walk doesn't
/// descend into).
pub(super) fn canonical_json_array(val: &str) -> String {
    if val.trim().is_empty() {
        return "[]".to_string();
    }

    // JSON API / validate endpoint — a JSON array. Each element is stringified
    // so a numeric/bool `has_many` (e.g. `[1, 2]`) isn't corrupted.
    if let Some(canonical) = parse_as_json_array(val) {
        return canonical;
    }

    // Any other text is one value — a single picked option, whose own commas
    // are part of the value.
    json!([val]).to_string()
}

/// If `val` parses as a JSON array, return a canonical JSON **string** array —
/// every element stringified (numbers/bools rendered as text, nulls dropped).
/// A non-array JSON value, or text that doesn't parse as JSON, returns `None`
/// so the caller treats the text as one value.
///
/// Elements are stringified rather than requiring all-strings: a number
/// `has_many` posts `[1, 2]` from the JSON endpoint, and comma-splitting the
/// raw text `"[1,2]"` would corrupt it into `["[1", "2]"]`.
fn parse_as_json_array(val: &str) -> Option<String> {
    let trimmed = val.trim_start();
    if !trimmed.starts_with('[') {
        return None;
    }

    let parsed: Value = serde_json::from_str(trimmed).ok()?;
    let arr = parsed.as_array()?;

    let strings: Vec<String> = arr
        .iter()
        .filter_map(|v| match v {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => Some(other.to_string()),
        })
        .collect();

    Some(json!(strings).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{FieldAdmin, FieldDefinition, FieldType, LocalizedString, SelectOption};
    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, ft).build()
    }

    #[test]
    fn transform_select_has_many_passes_a_repeated_key_array_through() {
        let mut form = HashMap::new();
        form.insert("tags".to_string(), r#"["red","blue","green"]"#.to_string());

        let mut field = make_field("tags", FieldType::Select);
        field.has_many = true;
        field.options = vec![
            SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
            SelectOption::new(LocalizedString::Plain("Blue".to_string()), "blue"),
            SelectOption::new(LocalizedString::Plain("Green".to_string()), "green"),
        ];

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("tags").unwrap(), r#"["red","blue","green"]"#);
    }

    /// Regression: a single picked option whose value holds a comma was split
    /// into two values.
    #[test]
    fn transform_select_has_many_keeps_a_single_value_with_a_comma_whole() {
        let mut form = HashMap::new();
        form.insert("sizes".to_string(), "10,5 cm".to_string());

        let mut field = make_field("sizes", FieldType::Select);
        field.has_many = true;

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("sizes").unwrap(), r#"["10,5 cm"]"#);
    }

    #[test]
    fn transform_select_has_many_empty_value() {
        let mut form = HashMap::new();
        form.insert("tags".to_string(), String::new());

        let mut field = make_field("tags", FieldType::Select);
        field.has_many = true;

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("tags").unwrap(), "[]");
    }

    #[test]
    fn transform_select_has_many_missing_key() {
        let mut form = HashMap::new();

        let mut field = make_field("tags", FieldType::Select);
        field.has_many = true;

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("tags").unwrap(), "[]");
    }

    #[test]
    fn transform_select_has_many_single_value() {
        let mut form = HashMap::new();
        form.insert("color".to_string(), "red".to_string());

        let mut field = make_field("color", FieldType::Select);
        field.has_many = true;

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("color").unwrap(), r#"["red"]"#);
    }

    #[test]
    fn transform_select_has_many_skips_non_has_many() {
        let mut form = HashMap::new();
        form.insert("status".to_string(), "active".to_string());

        let field = make_field("status", FieldType::Select);
        // has_many is false by default

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("status").unwrap(), "active"); // unchanged
    }

    #[test]
    fn transform_select_has_many_in_group() {
        let mut form = HashMap::new();
        form.insert("meta__tags".to_string(), r#"["a","b"]"#.to_string());

        let mut tag_field = make_field("tags", FieldType::Select);
        tag_field.has_many = true;

        let mut group = make_field("meta", FieldType::Group);
        group.fields = vec![tag_field];

        transform_select_has_many(&mut form, &[group]);
        assert_eq!(form.get("meta__tags").unwrap(), r#"["a","b"]"#);
    }

    #[test]
    fn transform_has_many_in_group_collapsible() {
        let mut form = HashMap::new();
        form.insert("config__tags".to_string(), r#"["a","b"]"#.to_string());

        let mut tag_field = make_field("tags", FieldType::Select);
        tag_field.has_many = true;
        let collapsible = FieldDefinition::builder("wrapper", FieldType::Collapsible)
            .fields(vec![tag_field])
            .build();
        let group = FieldDefinition::builder("config", FieldType::Group)
            .fields(vec![collapsible])
            .build();

        transform_select_has_many(&mut form, &[group]);
        assert_eq!(form.get("config__tags").unwrap(), r#"["a","b"]"#);
    }

    #[test]
    fn transform_has_many_in_nested_groups() {
        let mut form = HashMap::new();
        form.insert("outer__inner__tags".to_string(), r#"["x","y"]"#.to_string());

        let mut tag_field = make_field("tags", FieldType::Text);
        tag_field.has_many = true;
        let inner = FieldDefinition::builder("inner", FieldType::Group)
            .fields(vec![tag_field])
            .build();
        let outer = FieldDefinition::builder("outer", FieldType::Group)
            .fields(vec![inner])
            .build();

        transform_select_has_many(&mut form, &[outer]);
        assert_eq!(form.get("outer__inner__tags").unwrap(), r#"["x","y"]"#);
    }

    // Regression: `<crap-validate-form>` sends multi-select values as a JSON
    // array (after `values_to_string_map` serializes `Value::Array` via
    // `to_string`). Previously we split that on the embedded commas, turning
    // each JSON-quoted element into a literal string value and producing errors
    // like `skills has an invalid option: "motion"]`.
    #[test]
    fn transform_select_has_many_accepts_json_array_input() {
        let mut form = HashMap::new();
        form.insert(
            "skills".to_string(),
            r#"["design","motion","3d"]"#.to_string(),
        );

        let mut field = make_field("skills", FieldType::Select);
        field.has_many = true;

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(
            form.get("skills").unwrap(),
            r#"["design","motion","3d"]"#,
            "JSON array input must pass through unchanged, not be split on the quote-delimited commas",
        );
    }

    /// Regression: a numeric `has_many` posts a JSON array of numbers
    /// (`[1, 2]`). Previously `parse_as_json_string_array` rejected the
    /// non-string elements and the caller comma-split the raw text `"[1,2]"`
    /// into `["[1", "2]"]`. Each element must be stringified instead.
    #[test]
    fn transform_select_has_many_stringifies_numeric_array() {
        let mut form = HashMap::new();
        form.insert("scores".to_string(), "[1,2,3]".to_string());

        let mut field = make_field("scores", FieldType::Number);
        field.has_many = true;

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("scores").unwrap(), r#"["1","2","3"]"#);
    }

    #[test]
    fn transform_select_has_many_json_array_single_element() {
        let mut form = HashMap::new();
        form.insert("skills".to_string(), r#"["motion"]"#.to_string());

        let mut field = make_field("skills", FieldType::Select);
        field.has_many = true;

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("skills").unwrap(), r#"["motion"]"#);
    }

    /// An empty JSON array must be preserved as-is (not split on the brackets).
    #[test]
    fn transform_select_has_many_json_empty_array() {
        let mut form = HashMap::new();
        form.insert("skills".to_string(), "[]".to_string());

        let mut field = make_field("skills", FieldType::Select);
        field.has_many = true;

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("skills").unwrap(), "[]");
    }

    /// Regression: a `has_many` the form never rendered must stay absent, so
    /// the write keeps its stored list. Normalizing every `has_many` — rendered
    /// or not — replaced a hidden list with `[]` on every admin save.
    #[test]
    fn transform_select_has_many_skips_fields_the_form_never_rendered() {
        let mut hidden = FieldDefinition::builder("internal", FieldType::Select)
            .admin(FieldAdmin::builder().hidden(true).build())
            .build();
        hidden.has_many = true;

        let mut in_hidden_group = make_field("tags", FieldType::Text);
        in_hidden_group.has_many = true;
        let group = FieldDefinition::builder("system", FieldType::Group)
            .admin(FieldAdmin::builder().hidden(true).build())
            .fields(vec![in_hidden_group])
            .build();

        let mut form = HashMap::new();
        transform_select_has_many(&mut form, &[hidden, group]);

        assert!(
            form.is_empty(),
            "a hidden has-many keeps its stored list, got {form:?}"
        );
    }

    /// Text that merely *starts* with `[` but is no JSON array is one value.
    #[test]
    fn transform_select_has_many_bracket_prefixed_text_is_one_value() {
        let mut form = HashMap::new();
        form.insert("tags".to_string(), "[legacy,tag".to_string());

        let mut field = make_field("tags", FieldType::Select);
        field.has_many = true;

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("tags").unwrap(), r#"["[legacy,tag"]"#);
    }
}
