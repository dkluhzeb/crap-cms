//! Builds enriched child field contexts for layout wrappers (Row, Collapsible, Tabs)
//! inside Array and Blocks rows.

use std::collections::HashMap;

use serde_json::{Value, json};

use crate::{
    admin::handlers::{
        field_context::{
            MAX_FIELD_DEPTH,
            builder::{FieldRecursionCtx, apply_field_type_extras},
            count_errors_in_fields,
        },
        shared::auto_label_from_name,
    },
    core::field::{FieldDefinition, FieldType},
};

/// Build enriched child field contexts from structured JSON data.
/// Used by layout wrapper handlers (Tabs/Row/Collapsible) inside Array/Blocks
/// rows to correctly propagate structured data to nested layout wrappers.
///
/// For each child field:
/// - Layout wrappers get transparent names and the whole parent data object
/// - Leaf fields get `parent_name[field_name]` names and their specific value
/// - Recursion handles arbitrary nesting depth (Row inside Tabs inside Array, etc.)
pub fn build_enriched_children_from_data(
    fields: &[FieldDefinition],
    data: Option<&Value>,
    parent_name: &str,
    locale_locked: bool,
    non_default_locale: bool,
    depth: usize,
    errors: &HashMap<String, String>,
) -> Vec<Value> {
    if depth >= MAX_FIELD_DEPTH {
        return Vec::new();
    }

    let data_obj = data.and_then(|v| v.as_object());

    fields
        .iter()
        .map(|child| {
            let is_wrapper = matches!(
                child.field_type,
                FieldType::Tabs | FieldType::Row | FieldType::Collapsible
            );

            let child_raw = if is_wrapper {
                data // pass whole object
            } else {
                data_obj.and_then(|m| m.get(&child.name))
            };

            let child_name = if is_wrapper {
                parent_name.to_string() // transparent
            } else {
                format!("{}[{}]", parent_name, child.name)
            };

            let child_val = child_raw
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => {
                        if is_wrapper {
                            String::new()
                        } else {
                            other.to_string()
                        }
                    }
                })
                .unwrap_or_default();

            let child_label = child
                .admin
                .label
                .as_ref()
                .map(|ls| ls.resolve_default().to_string())
                .unwrap_or_else(|| auto_label_from_name(&child.name));

            let mut child_ctx = json!({
                "name": child_name,
                "field_type": child.field_type.as_str(),
                "label": child_label,
                "value": child_val,
                "required": child.required,
                "readonly": child.admin.readonly || locale_locked,
                "locale_locked": locale_locked,
                "placeholder": child.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
                "description": child.admin.description.as_ref().map(|ls| ls.resolve_default()),
            });

            if let Some(err) = errors.get(&child_name) {
                child_ctx["error"] = json!(err);
            }

            match child.field_type {
                FieldType::Row | FieldType::Collapsible => {
                    let sub_fields = build_enriched_children_from_data(
                        &child.fields,
                        child_raw,
                        &child_name,
                        locale_locked,
                        non_default_locale,
                        depth + 1,
                        errors,
                    );

                    child_ctx["sub_fields"] = json!(sub_fields);

                    if child.field_type == FieldType::Collapsible {
                        child_ctx["collapsed"] = json!(child.admin.collapsed);
                    }
                }
                FieldType::Group => {
                    // Group inside Array row: add [0] index for form parser compatibility
                    let group_prefix = format!("{}[0]", child_name);
                    let sub_fields = build_enriched_children_from_data(
                        &child.fields,
                        child_raw,
                        &group_prefix,
                        locale_locked,
                        non_default_locale,
                        depth + 1,
                        errors,
                    );

                    child_ctx["sub_fields"] = json!(sub_fields);
                    child_ctx["collapsed"] = json!(child.admin.collapsed);
                }
                FieldType::Tabs => {
                    let tabs_ctx: Vec<_> = child
                        .tabs
                        .iter()
                        .map(|tab| {
                            let tab_sub_fields = build_enriched_children_from_data(
                                &tab.fields,
                                child_raw,
                                &child_name,
                                locale_locked,
                                non_default_locale,
                                depth + 1,
                                errors,
                            );

                            let error_count = count_errors_in_fields(&tab_sub_fields);

                            let mut tab_ctx = json!({
                                "label": &tab.label,
                                "sub_fields": tab_sub_fields,
                            });

                            if error_count > 0 {
                                tab_ctx["error_count"] = json!(error_count);
                            }

                            if let Some(ref desc) = tab.description {
                                tab_ctx["description"] = json!(desc);
                            }

                            tab_ctx
                        })
                        .collect();

                    child_ctx["tabs"] = json!(tabs_ctx);
                }
                _ => {
                    let empty = HashMap::new();
                    let extras_ctx = FieldRecursionCtx::builder(&empty, errors, &child_name)
                        .non_default_locale(non_default_locale)
                        .depth(depth + 1)
                        .build();

                    apply_field_type_extras(child, &child_val, &mut child_ctx, &extras_ctx);

                    // Inject stored timezone value from parent row for Date fields
                    if child.field_type == FieldType::Date && child.timezone {
                        let tz_key = format!("{}_tz", child.name);
                        let tz_val = data_obj
                            .and_then(|m| m.get(&tz_key))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        if !tz_val.is_empty() {
                            child_ctx["timezone_value"] = json!(tz_val);
                        }
                    }
                }
            }

            child_ctx
        })
        .collect()
}
