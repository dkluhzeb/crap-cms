//! Normalize `has_many` select/text/number form values into JSON array strings.
//!
//! Two input shapes are accepted:
//! - Comma-separated (`"a,b,c"`) — produced by traditional HTML form submission
//!   after `parse_form` collapses duplicate keys into a joined string.
//! - JSON array (`"[\"a\",\"b\"]"`) — produced by the `<crap-validate-form>` JSON
//!   endpoint, where `values_to_string_map` serializes array values with
//!   `Value::to_string()`. Forwarding these intact avoids a double-encoding bug
//!   where each JSON-quoted element (`"a"`) was split on the trailing comma and
//!   re-wrapped as a literal string value.

use serde_json::{Value, json};
use std::collections::HashMap;

use crate::core::field::{FieldDefinition, FieldType};

/// Normalize `has_many` select/text/number form values into canonical JSON array strings.
pub(crate) fn transform_select_has_many(
    form: &mut HashMap<String, String>,
    field_defs: &[FieldDefinition],
) {
    transform_has_many_recursive(form, field_defs, "");
}

/// If `val` already parses as a JSON array of strings, return it re-serialized
/// in canonical form. Mixed / non-string arrays and non-array JSON return `None`
/// and the caller falls back to comma-separated parsing.
fn parse_as_json_string_array(val: &str) -> Option<String> {
    let trimmed = val.trim_start();
    if !trimmed.starts_with('[') {
        return None;
    }

    let parsed: Value = serde_json::from_str(trimmed).ok()?;
    let arr = parsed.as_array()?;

    let strings: Vec<&str> = arr.iter().map(|v| v.as_str()).collect::<Option<_>>()?;

    Some(json!(strings).to_string())
}

/// Recursive helper for `transform_select_has_many`.
/// `prefix` accumulates `__`-separated Group names.
/// Layout wrappers (Row/Collapsible/Tabs) pass through transparently.
fn transform_has_many_recursive(
    form: &mut HashMap<String, String>,
    field_defs: &[FieldDefinition],
    prefix: &str,
) {
    // Collect transforms first to avoid double-borrow on `form`
    let mut updates: Vec<(String, String)> = Vec::new();

    for field in field_defs {
        let full_name = if prefix.is_empty() {
            field.name.clone()
        } else {
            format!("{}__{}", prefix, field.name)
        };

        match field.field_type {
            FieldType::Select | FieldType::Text | FieldType::Number if field.has_many => {
                if let Some(val) = form.get(&full_name) {
                    let json_val = if val.is_empty() {
                        "[]".to_string()
                    } else if let Some(canonical) = parse_as_json_string_array(val) {
                        // JSON API / validate endpoint — already an array of strings.
                        canonical
                    } else {
                        // Traditional HTML form — comma-separated.
                        let values: Vec<&str> = val
                            .split(',')
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty())
                            .collect();

                        json!(values).to_string()
                    };
                    updates.push((full_name, json_val));
                } else {
                    updates.push((full_name, "[]".to_string()));
                }
            }
            FieldType::Group => {
                transform_has_many_recursive(form, &field.fields, &full_name);
            }
            FieldType::Row | FieldType::Collapsible => {
                transform_has_many_recursive(form, &field.fields, prefix);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    transform_has_many_recursive(form, &tab.fields, prefix);
                }
            }
            _ => {}
        }
    }

    for (name, val) in updates {
        form.insert(name, val);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::{FieldDefinition, FieldType, LocalizedString, SelectOption};

    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition::builder(name, ft).build()
    }

    #[test]
    fn transform_select_has_many_converts_comma_separated() {
        let mut form = HashMap::new();
        form.insert("tags".to_string(), "red,blue,green".to_string());

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
        form.insert("meta__tags".to_string(), "a,b".to_string());

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
        form.insert("config__tags".to_string(), "a,b".to_string());

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
        form.insert("outer__inner__tags".to_string(), "x,y".to_string());

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

    /// A comma-separated value that merely *starts* with `[` must not be
    /// misidentified as JSON — fall back to comma splitting.
    #[test]
    fn transform_select_has_many_comma_separated_with_bracket_prefix() {
        let mut form = HashMap::new();
        form.insert("tags".to_string(), "[legacy,tag".to_string());

        let mut field = make_field("tags", FieldType::Select);
        field.has_many = true;

        transform_select_has_many(&mut form, &[field]);
        assert_eq!(form.get("tags").unwrap(), r#"["[legacy","tag"]"#);
    }
}
