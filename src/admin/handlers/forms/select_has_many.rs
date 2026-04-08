//! Transform comma-separated form values to JSON arrays for `has_many` select fields.

use serde_json::json;
use std::collections::HashMap;

use crate::core::field::{FieldDefinition, FieldType};

/// Convert comma-separated form values to JSON arrays for `has_many` select fields.
/// The JS multi-select interceptor joins selected values with commas; this converts
/// them to JSON array strings (e.g., `"a,b"` → `'["a","b"]'`) for storage in TEXT columns.
pub(crate) fn transform_select_has_many(
    form: &mut HashMap<String, String>,
    field_defs: &[FieldDefinition],
) {
    transform_has_many_recursive(form, field_defs, "");
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
                    } else {
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
}
