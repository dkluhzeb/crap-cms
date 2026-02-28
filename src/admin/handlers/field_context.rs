//! Field context builders for admin form rendering.
//! Builds template context objects from field definitions, handling recursive
//! composite types (Array, Blocks, Group) with nesting depth limits.

use std::collections::HashMap;
use crate::core::field::FieldType;

use super::shared::{auto_label_from_name, compute_row_label};

/// Make a template-ID-safe string from a field name (replaces `[`, `]` with `-`).
fn safe_template_id(name: &str) -> String {
    name.replace('[', "-").replace(']', "")
}

/// Max nesting depth for recursive field context building (guard against infinite nesting).
const MAX_FIELD_DEPTH: usize = 5;

/// Build a field context for a single field definition, recursing into composite sub-fields.
///
/// `name_prefix`: the full form-name prefix for this field (e.g. `"content[0]"` for a
/// field inside a blocks row at index 0). Top-level fields use an empty prefix.
/// `depth`: current nesting depth (0 = top-level). Stops recursing at MAX_FIELD_DEPTH.
fn build_single_field_context(
    field: &crate::core::field::FieldDefinition,
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    name_prefix: &str,
    non_default_locale: bool,
    depth: usize,
) -> serde_json::Value {
    let full_name = if name_prefix.is_empty() {
        field.name.clone()
    } else {
        format!("{}[{}]", name_prefix, field.name)
    };
    let value = values.get(&full_name).cloned().unwrap_or_default();
    let label = field.admin.label.as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| auto_label_from_name(&field.name));
    let locale_locked = non_default_locale && !field.localized;

    let mut ctx = serde_json::json!({
        "name": full_name,
        "field_type": field.field_type.as_str(),
        "label": label,
        "required": field.required,
        "value": value,
        "placeholder": field.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
        "description": field.admin.description.as_ref().map(|ls| ls.resolve_default()),
        "readonly": field.admin.readonly || locale_locked,
        "localized": field.localized,
        "locale_locked": locale_locked,
    });

    if let Some(ref pos) = field.admin.position {
        ctx["position"] = serde_json::json!(pos);
    }

    if let Some(err) = errors.get(&full_name) {
        ctx["error"] = serde_json::json!(err);
    }

    // Beyond max depth, render as a simple text input
    if depth >= MAX_FIELD_DEPTH {
        return ctx;
    }

    match &field.field_type {
        FieldType::Select => {
            let options: Vec<_> = field.options.iter().map(|opt| {
                serde_json::json!({
                    "label": opt.label.resolve_default(),
                    "value": opt.value,
                    "selected": opt.value == value,
                })
            }).collect();
            ctx["options"] = serde_json::json!(options);
        }
        FieldType::Checkbox => {
            let checked = matches!(value.as_str(), "1" | "true" | "on" | "yes");
            ctx["checked"] = serde_json::json!(checked);
        }
        FieldType::Relationship => {
            if let Some(ref rc) = field.relationship {
                ctx["relationship_collection"] = serde_json::json!(rc.collection);
                ctx["has_many"] = serde_json::json!(rc.has_many);
            }
        }
        FieldType::Array => {
            // Build sub_field contexts for the <template> section (with __INDEX__ placeholder)
            let template_prefix = format!("{}[__INDEX__]", full_name);
            let sub_fields: Vec<_> = field.fields.iter().map(|sf| {
                build_single_field_context(sf, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
            }).collect();
            ctx["sub_fields"] = serde_json::json!(sub_fields);
            ctx["row_count"] = serde_json::json!(0);
            ctx["template_id"] = serde_json::json!(safe_template_id(&full_name));
            if let Some(ref lf) = field.admin.label_field {
                ctx["label_field"] = serde_json::json!(lf);
            }
            if let Some(max) = field.max_rows {
                ctx["max_rows"] = serde_json::json!(max);
            }
            if let Some(min) = field.min_rows {
                ctx["min_rows"] = serde_json::json!(min);
            }
            if field.admin.init_collapsed {
                ctx["init_collapsed"] = serde_json::json!(true);
            }
            if let Some(ref ls) = field.admin.labels_singular {
                ctx["add_label"] = serde_json::json!(ls.resolve_default());
            }
        }
        FieldType::Group => {
            // Group sub-fields use double-underscore naming at top level,
            // but when nested inside Array/Blocks they use bracketed names.
            let sub_fields: Vec<_> = if name_prefix.is_empty() {
                // Top-level group: use col_name pattern (group__subfield)
                field.fields.iter().map(|sf| {
                    let col_name = format!("{}__{}", field.name, sf.name);
                    let sub_value = values.get(&col_name).cloned().unwrap_or_default();
                    let sub_label = sf.admin.label.as_ref()
                        .map(|ls| ls.resolve_default().to_string())
                        .unwrap_or_else(|| auto_label_from_name(&sf.name));
                    let sf_locale_locked = non_default_locale && !field.localized;
                    let mut sub_ctx = serde_json::json!({
                        "name": col_name,
                        "field_type": sf.field_type.as_str(),
                        "label": sub_label,
                        "required": sf.required,
                        "value": sub_value,
                        "placeholder": sf.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
                        "description": sf.admin.description.as_ref().map(|ls| ls.resolve_default()),
                        "readonly": sf.admin.readonly || sf_locale_locked,
                        "localized": field.localized,
                        "locale_locked": sf_locale_locked,
                    });
                    // Recurse for nested composites
                    apply_field_type_extras(sf, &sub_value, &mut sub_ctx, values, errors, &col_name, non_default_locale, depth + 1);
                    sub_ctx
                }).collect()
            } else {
                // Nested group: use bracketed naming via recursion
                field.fields.iter().map(|sf| {
                    build_single_field_context(sf, values, errors, &full_name, non_default_locale, depth + 1)
                }).collect()
            };
            ctx["sub_fields"] = serde_json::json!(sub_fields);
            if field.admin.collapsed {
                ctx["collapsed"] = serde_json::json!(true);
            }
        }
        FieldType::Row => {
            // Row is a layout-only container; sub-fields are promoted to top level.
            // Top-level row promotes sub-fields to the same level as the parent,
            // so we delegate to build_single_field_context with the same prefix.
            // This correctly handles Group (double-underscore), Collapsible, etc.
            let sub_fields: Vec<_> = if name_prefix.is_empty() {
                field.fields.iter().map(|sf| {
                    build_single_field_context(sf, values, errors, "", non_default_locale, depth + 1)
                }).collect()
            } else {
                // Nested row: use bracketed naming via recursion
                field.fields.iter().map(|sf| {
                    build_single_field_context(sf, values, errors, &full_name, non_default_locale, depth + 1)
                }).collect()
            };
            ctx["sub_fields"] = serde_json::json!(sub_fields);
        }
        FieldType::Collapsible => {
            // Collapsible is a layout-only container like Row but with a toggle header.
            // Top-level collapsible promotes sub-fields to the same level as the parent,
            // so we delegate to build_single_field_context with the same prefix.
            // This correctly handles Group (double-underscore), Row, etc.
            let sub_fields: Vec<_> = if name_prefix.is_empty() {
                field.fields.iter().map(|sf| {
                    build_single_field_context(sf, values, errors, "", non_default_locale, depth + 1)
                }).collect()
            } else {
                field.fields.iter().map(|sf| {
                    build_single_field_context(sf, values, errors, &full_name, non_default_locale, depth + 1)
                }).collect()
            };
            ctx["sub_fields"] = serde_json::json!(sub_fields);
            if field.admin.collapsed {
                ctx["collapsed"] = serde_json::json!(true);
            }
        }
        FieldType::Tabs => {
            // Tabs is a layout-only container with multiple tab panels.
            // Top-level tabs promote sub-fields to the same level as the parent,
            // so we delegate to build_single_field_context with the same prefix.
            // This correctly handles Group (double-underscore), Row, Collapsible, etc.
            let tabs_ctx: Vec<_> = field.tabs.iter().map(|tab| {
                let tab_sub_fields: Vec<_> = if name_prefix.is_empty() {
                    tab.fields.iter().map(|sf| {
                        build_single_field_context(sf, values, errors, "", non_default_locale, depth + 1)
                    }).collect()
                } else {
                    tab.fields.iter().map(|sf| {
                        build_single_field_context(sf, values, errors, &full_name, non_default_locale, depth + 1)
                    }).collect()
                };
                let mut tab_ctx = serde_json::json!({
                    "label": &tab.label,
                    "sub_fields": tab_sub_fields,
                });
                if let Some(ref desc) = tab.description {
                    tab_ctx["description"] = serde_json::json!(desc);
                }
                tab_ctx
            }).collect();
            ctx["tabs"] = serde_json::json!(tabs_ctx);
        }
        FieldType::Date => {
            let appearance = field.picker_appearance.as_deref().unwrap_or("dayOnly");
            ctx["picker_appearance"] = serde_json::json!(appearance);
            match appearance {
                "dayOnly" => {
                    let date_val = if value.len() >= 10 { &value[..10] } else { &value };
                    ctx["date_only_value"] = serde_json::json!(date_val);
                }
                "dayAndTime" => {
                    let dt_val = if value.len() >= 16 { &value[..16] } else { &value };
                    ctx["datetime_local_value"] = serde_json::json!(dt_val);
                }
                _ => {}
            }
        }
        FieldType::Upload => {
            if let Some(ref rc) = field.relationship {
                ctx["relationship_collection"] = serde_json::json!(rc.collection);
            }
        }
        FieldType::Blocks => {
            let block_defs: Vec<_> = field.blocks.iter().map(|bd| {
                // Build sub-field contexts for each block type's <template> section
                let template_prefix = format!("{}[__INDEX__]", full_name);
                let block_fields: Vec<_> = bd.fields.iter().map(|sf| {
                    build_single_field_context(sf, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
                }).collect();
                let mut def = serde_json::json!({
                    "block_type": bd.block_type,
                    "label": bd.label.as_ref().map(|ls| ls.resolve_default()).unwrap_or(&bd.block_type),
                    "fields": block_fields,
                });
                if let Some(ref lf) = bd.label_field {
                    def["label_field"] = serde_json::json!(lf);
                }
                def
            }).collect();
            ctx["block_definitions"] = serde_json::json!(block_defs);
            ctx["row_count"] = serde_json::json!(0);
            ctx["template_id"] = serde_json::json!(safe_template_id(&full_name));
            if let Some(ref lf) = field.admin.label_field {
                ctx["label_field"] = serde_json::json!(lf);
            }
            if let Some(max) = field.max_rows {
                ctx["max_rows"] = serde_json::json!(max);
            }
            if let Some(min) = field.min_rows {
                ctx["min_rows"] = serde_json::json!(min);
            }
            if field.admin.init_collapsed {
                ctx["init_collapsed"] = serde_json::json!(true);
            }
            if let Some(ref ls) = field.admin.labels_singular {
                ctx["add_label"] = serde_json::json!(ls.resolve_default());
            }
        }
        _ => {}
    }

    ctx
}

/// Apply type-specific extras to an already-built sub_ctx (for top-level group sub-fields
/// that use the `col_name` pattern but still need composite-type recursion).
fn apply_field_type_extras(
    sf: &crate::core::field::FieldDefinition,
    value: &str,
    sub_ctx: &mut serde_json::Value,
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    name_prefix: &str,
    non_default_locale: bool,
    depth: usize,
) {
    if depth >= MAX_FIELD_DEPTH { return; }
    match &sf.field_type {
        FieldType::Checkbox => {
            let checked = matches!(value, "1" | "true" | "on" | "yes");
            sub_ctx["checked"] = serde_json::json!(checked);
        }
        FieldType::Select => {
            let options: Vec<_> = sf.options.iter().map(|opt| {
                serde_json::json!({
                    "label": opt.label.resolve_default(),
                    "value": opt.value,
                    "selected": opt.value == value,
                })
            }).collect();
            sub_ctx["options"] = serde_json::json!(options);
        }
        FieldType::Date => {
            let appearance = sf.picker_appearance.as_deref().unwrap_or("dayOnly");
            sub_ctx["picker_appearance"] = serde_json::json!(appearance);
            match appearance {
                "dayOnly" => {
                    let date_val = if value.len() >= 10 { &value[..10] } else { value };
                    sub_ctx["date_only_value"] = serde_json::json!(date_val);
                }
                "dayAndTime" => {
                    let dt_val = if value.len() >= 16 { &value[..16] } else { value };
                    sub_ctx["datetime_local_value"] = serde_json::json!(dt_val);
                }
                _ => {}
            }
        }
        FieldType::Array => {
            let template_prefix = format!("{}[__INDEX__]", name_prefix);
            let sub_fields: Vec<_> = sf.fields.iter().map(|nested| {
                build_single_field_context(nested, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
            }).collect();
            sub_ctx["sub_fields"] = serde_json::json!(sub_fields);
            sub_ctx["row_count"] = serde_json::json!(0);
            sub_ctx["template_id"] = serde_json::json!(safe_template_id(name_prefix));
            if let Some(ref lf) = sf.admin.label_field {
                sub_ctx["label_field"] = serde_json::json!(lf);
            }
            if let Some(max) = sf.max_rows {
                sub_ctx["max_rows"] = serde_json::json!(max);
            }
            if let Some(min) = sf.min_rows {
                sub_ctx["min_rows"] = serde_json::json!(min);
            }
            if sf.admin.init_collapsed {
                sub_ctx["init_collapsed"] = serde_json::json!(true);
            }
            if let Some(ref ls) = sf.admin.labels_singular {
                sub_ctx["add_label"] = serde_json::json!(ls.resolve_default());
            }
        }
        FieldType::Group => {
            let sub_fields: Vec<_> = sf.fields.iter().map(|nested| {
                build_single_field_context(nested, values, errors, name_prefix, non_default_locale, depth + 1)
            }).collect();
            sub_ctx["sub_fields"] = serde_json::json!(sub_fields);
            if sf.admin.collapsed {
                sub_ctx["collapsed"] = serde_json::json!(true);
            }
        }
        FieldType::Row => {
            let sub_fields: Vec<_> = sf.fields.iter().map(|nested| {
                build_single_field_context(nested, values, errors, name_prefix, non_default_locale, depth + 1)
            }).collect();
            sub_ctx["sub_fields"] = serde_json::json!(sub_fields);
        }
        FieldType::Collapsible => {
            let sub_fields: Vec<_> = sf.fields.iter().map(|nested| {
                build_single_field_context(nested, values, errors, name_prefix, non_default_locale, depth + 1)
            }).collect();
            sub_ctx["sub_fields"] = serde_json::json!(sub_fields);
            if sf.admin.collapsed {
                sub_ctx["collapsed"] = serde_json::json!(true);
            }
        }
        FieldType::Tabs => {
            let tabs_ctx: Vec<_> = sf.tabs.iter().map(|tab| {
                let tab_sub_fields: Vec<_> = tab.fields.iter().map(|nested| {
                    build_single_field_context(nested, values, errors, name_prefix, non_default_locale, depth + 1)
                }).collect();
                let mut tab_ctx = serde_json::json!({
                    "label": &tab.label,
                    "sub_fields": tab_sub_fields,
                });
                if let Some(ref desc) = tab.description {
                    tab_ctx["description"] = serde_json::json!(desc);
                }
                tab_ctx
            }).collect();
            sub_ctx["tabs"] = serde_json::json!(tabs_ctx);
        }
        FieldType::Blocks => {
            let block_defs: Vec<_> = sf.blocks.iter().map(|bd| {
                let template_prefix = format!("{}[__INDEX__]", name_prefix);
                let block_fields: Vec<_> = bd.fields.iter().map(|nested| {
                    build_single_field_context(nested, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
                }).collect();
                let mut def = serde_json::json!({
                    "block_type": bd.block_type,
                    "label": bd.label.as_ref().map(|ls| ls.resolve_default()).unwrap_or(&bd.block_type),
                    "fields": block_fields,
                });
                if let Some(ref lf) = bd.label_field {
                    def["label_field"] = serde_json::json!(lf);
                }
                def
            }).collect();
            sub_ctx["block_definitions"] = serde_json::json!(block_defs);
            sub_ctx["row_count"] = serde_json::json!(0);
            sub_ctx["template_id"] = serde_json::json!(safe_template_id(name_prefix));
            if let Some(max) = sf.max_rows {
                sub_ctx["max_rows"] = serde_json::json!(max);
            }
            if let Some(min) = sf.min_rows {
                sub_ctx["min_rows"] = serde_json::json!(min);
            }
            if sf.admin.init_collapsed {
                sub_ctx["init_collapsed"] = serde_json::json!(true);
            }
            if let Some(ref ls) = sf.admin.labels_singular {
                sub_ctx["add_label"] = serde_json::json!(ls.resolve_default());
            }
        }
        FieldType::Relationship => {
            if let Some(ref rc) = sf.relationship {
                sub_ctx["relationship_collection"] = serde_json::json!(rc.collection);
                sub_ctx["has_many"] = serde_json::json!(rc.has_many);
            }
        }
        FieldType::Upload => {
            if let Some(ref rc) = sf.relationship {
                sub_ctx["relationship_collection"] = serde_json::json!(rc.collection);
            }
        }
        _ => {}
    }
}

/// Build field context objects for template rendering.
///
/// `non_default_locale`: when true, non-localized fields are rendered readonly
/// (locked) because they are shared across all locales and should only be edited
/// from the default locale.
pub(super) fn build_field_contexts(
    fields: &[crate::core::field::FieldDefinition],
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    filter_hidden: bool,
    non_default_locale: bool,
) -> Vec<serde_json::Value> {
    let iter: Box<dyn Iterator<Item = &crate::core::field::FieldDefinition>> = if filter_hidden {
        Box::new(fields.iter().filter(|field| !field.admin.hidden))
    } else {
        Box::new(fields.iter())
    };
    iter.map(|field| {
        build_single_field_context(field, values, errors, "", non_default_locale, 0)
    }).collect()
}

/// Evaluate display conditions for field contexts and inject condition data.
/// For fields with `admin.condition`, calls the Lua function and sets:
/// - `condition_visible`: initial visibility (bool)
/// - `condition_json`: condition table for client-side evaluation (if table returned)
/// - `condition_ref`: Lua function ref for server-side evaluation (if bool returned)
pub(super) fn apply_display_conditions(
    fields: &mut [serde_json::Value],
    field_defs: &[crate::core::field::FieldDefinition],
    form_data: &serde_json::Value,
    hook_runner: &crate::hooks::lifecycle::HookRunner,
    filter_hidden: bool,
) {
    use crate::hooks::lifecycle::DisplayConditionResult;

    let defs_iter: Box<dyn Iterator<Item = &crate::core::field::FieldDefinition>> = if filter_hidden {
        Box::new(field_defs.iter().filter(|f| !f.admin.hidden))
    } else {
        Box::new(field_defs.iter())
    };

    for (ctx, field_def) in fields.iter_mut().zip(defs_iter) {
        if let Some(ref cond_ref) = field_def.admin.condition {
            match hook_runner.call_display_condition(cond_ref, form_data) {
                Some(DisplayConditionResult::Bool(visible)) => {
                    ctx["condition_visible"] = serde_json::json!(visible);
                    ctx["condition_ref"] = serde_json::json!(cond_ref);
                }
                Some(DisplayConditionResult::Table { condition, visible }) => {
                    ctx["condition_visible"] = serde_json::json!(visible);
                    ctx["condition_json"] = condition;
                }
                None => {
                    // Lua error -> show field (safe default)
                }
            }
        }
    }
}

/// Split field contexts into main and sidebar based on the `position` property.
/// Returns `(main_fields, sidebar_fields)`.
pub(super) fn split_sidebar_fields(
    fields: Vec<serde_json::Value>,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    fields.into_iter().partition(|f| {
        f.get("position").and_then(|v| v.as_str()) != Some("sidebar")
    })
}

/// Build a sub-field context for a single field within an array/blocks row,
/// recursively handling nested composite sub-fields.
///
/// `sf`: the sub-field definition
/// `raw_value`: the raw JSON value for this sub-field from the hydrated document
/// `parent_name`: the parent field's name (e.g. "content")
/// `idx`: the row index within the parent
/// `locale_locked`: whether the parent is locale-locked
/// `non_default_locale`: whether we're on a non-default locale
/// `depth`: nesting depth
fn build_enriched_sub_field_context(
    sf: &crate::core::field::FieldDefinition,
    raw_value: Option<&serde_json::Value>,
    parent_name: &str,
    idx: usize,
    locale_locked: bool,
    non_default_locale: bool,
    depth: usize,
    errors: &HashMap<String, String>,
) -> serde_json::Value {
    let indexed_name = format!("{}[{}][{}]", parent_name, idx, sf.name);

    // For scalar types, stringify the value. For composites, keep structured.
    let val = raw_value
        .map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Null => String::new(),
            other => {
                match sf.field_type {
                    FieldType::Array | FieldType::Blocks | FieldType::Group | FieldType::Row | FieldType::Collapsible | FieldType::Tabs => String::new(),
                    _ => other.to_string(),
                }
            }
        })
        .unwrap_or_default();

    let sf_label = sf.admin.label.as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| auto_label_from_name(&sf.name));

    let mut sub_ctx = serde_json::json!({
        "name": indexed_name,
        "field_type": sf.field_type.as_str(),
        "label": sf_label,
        "value": val,
        "required": sf.required,
        "readonly": sf.admin.readonly || locale_locked,
        "locale_locked": locale_locked,
        "placeholder": sf.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
        "description": sf.admin.description.as_ref().map(|ls| ls.resolve_default()),
    });

    if let Some(err) = errors.get(&indexed_name) {
        sub_ctx["error"] = serde_json::json!(err);
    }

    if depth >= MAX_FIELD_DEPTH { return sub_ctx; }

    match &sf.field_type {
        FieldType::Checkbox => {
            let checked = matches!(val.as_str(), "1" | "true" | "on" | "yes");
            sub_ctx["checked"] = serde_json::json!(checked);
        }
        FieldType::Select => {
            let options: Vec<_> = sf.options.iter().map(|opt| {
                serde_json::json!({
                    "label": opt.label.resolve_default(),
                    "value": opt.value,
                    "selected": opt.value == val,
                })
            }).collect();
            sub_ctx["options"] = serde_json::json!(options);
        }
        FieldType::Date => {
            let appearance = sf.picker_appearance.as_deref().unwrap_or("dayOnly");
            sub_ctx["picker_appearance"] = serde_json::json!(appearance);
            match appearance {
                "dayOnly" => {
                    let date_val = if val.len() >= 10 { &val[..10] } else { &val };
                    sub_ctx["date_only_value"] = serde_json::json!(date_val);
                }
                "dayAndTime" => {
                    let dt_val = if val.len() >= 16 { &val[..16] } else { &val };
                    sub_ctx["datetime_local_value"] = serde_json::json!(dt_val);
                }
                _ => {}
            }
        }
        FieldType::Relationship => {
            if let Some(ref rc) = sf.relationship {
                sub_ctx["relationship_collection"] = serde_json::json!(rc.collection);
                sub_ctx["has_many"] = serde_json::json!(rc.has_many);
            }
        }
        FieldType::Upload => {
            if let Some(ref rc) = sf.relationship {
                sub_ctx["relationship_collection"] = serde_json::json!(rc.collection);
            }
        }
        FieldType::Array => {
            // Nested array: recurse into sub-rows
            let nested_rows: Vec<serde_json::Value> = match raw_value {
                Some(serde_json::Value::Array(arr)) => {
                    arr.iter().enumerate().map(|(nested_idx, nested_row)| {
                        let nested_row_obj = nested_row.as_object();
                        let nested_sub_values: Vec<_> = sf.fields.iter().map(|nested_sf| {
                            let nested_raw = nested_row_obj.and_then(|m| m.get(&nested_sf.name));
                            build_enriched_sub_field_context(
                                nested_sf, nested_raw, &indexed_name, nested_idx,
                                locale_locked, non_default_locale, depth + 1, errors,
                            )
                        }).collect();
                        let row_has_errors = nested_sub_values.iter()
                            .any(|sf_ctx| sf_ctx.get("error").is_some());
                        let mut row_json = serde_json::json!({
                            "index": nested_idx,
                            "sub_fields": nested_sub_values,
                        });
                        if row_has_errors {
                            row_json["has_errors"] = serde_json::json!(true);
                        }
                        row_json
                    }).collect()
                }
                _ => Vec::new(),
            };
            // Template sub_fields for the nested <template> section
            let template_prefix = format!("{}[__INDEX__]", indexed_name);
            let template_sub_fields: Vec<_> = sf.fields.iter().map(|nested_sf| {
                build_single_field_context(nested_sf, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
            }).collect();
            sub_ctx["sub_fields"] = serde_json::json!(template_sub_fields);
            sub_ctx["rows"] = serde_json::json!(nested_rows);
            sub_ctx["row_count"] = serde_json::json!(nested_rows.len());
            sub_ctx["template_id"] = serde_json::json!(safe_template_id(&indexed_name));
            if let Some(max) = sf.max_rows {
                sub_ctx["max_rows"] = serde_json::json!(max);
            }
            if let Some(min) = sf.min_rows {
                sub_ctx["min_rows"] = serde_json::json!(min);
            }
            if sf.admin.init_collapsed {
                sub_ctx["init_collapsed"] = serde_json::json!(true);
            }
            if let Some(ref ls) = sf.admin.labels_singular {
                sub_ctx["add_label"] = serde_json::json!(ls.resolve_default());
            }
        }
        FieldType::Blocks => {
            // Nested blocks: recurse into block rows
            let nested_rows: Vec<serde_json::Value> = match raw_value {
                Some(serde_json::Value::Array(arr)) => {
                    arr.iter().enumerate().map(|(nested_idx, nested_row)| {
                        let nested_row_obj = nested_row.as_object();
                        let block_type = nested_row_obj
                            .and_then(|m| m.get("_block_type"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let block_label = sf.blocks.iter()
                            .find(|bd| bd.block_type == block_type)
                            .and_then(|bd| bd.label.as_ref().map(|ls| ls.resolve_default()))
                            .unwrap_or(block_type);
                        let block_def = sf.blocks.iter().find(|bd| bd.block_type == block_type);
                        let nested_sub_values: Vec<_> = block_def
                            .map(|bd| bd.fields.iter().map(|nested_sf| {
                                let nested_raw = nested_row_obj.and_then(|m| m.get(&nested_sf.name));
                                build_enriched_sub_field_context(
                                    nested_sf, nested_raw, &indexed_name, nested_idx,
                                    locale_locked, non_default_locale, depth + 1, errors,
                                )
                            }).collect())
                            .unwrap_or_default();
                        let row_has_errors = nested_sub_values.iter()
                            .any(|sf_ctx| sf_ctx.get("error").is_some());
                        let mut row_json = serde_json::json!({
                            "index": nested_idx,
                            "_block_type": block_type,
                            "block_label": block_label,
                            "sub_fields": nested_sub_values,
                        });
                        if row_has_errors {
                            row_json["has_errors"] = serde_json::json!(true);
                        }
                        row_json
                    }).collect()
                }
                _ => Vec::new(),
            };
            // Block definitions for the nested <template> sections
            let block_defs: Vec<_> = sf.blocks.iter().map(|bd| {
                let template_prefix = format!("{}[__INDEX__]", indexed_name);
                let block_fields: Vec<_> = bd.fields.iter().map(|nested_sf| {
                    build_single_field_context(nested_sf, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
                }).collect();
                let mut def = serde_json::json!({
                    "block_type": bd.block_type,
                    "label": bd.label.as_ref().map(|ls| ls.resolve_default()).unwrap_or(&bd.block_type),
                    "fields": block_fields,
                });
                if let Some(ref lf) = bd.label_field {
                    def["label_field"] = serde_json::json!(lf);
                }
                def
            }).collect();
            sub_ctx["block_definitions"] = serde_json::json!(block_defs);
            sub_ctx["rows"] = serde_json::json!(nested_rows);
            sub_ctx["row_count"] = serde_json::json!(nested_rows.len());
            sub_ctx["template_id"] = serde_json::json!(safe_template_id(&indexed_name));
            if let Some(ref lf) = sf.admin.label_field {
                sub_ctx["label_field"] = serde_json::json!(lf);
            }
            if let Some(max) = sf.max_rows {
                sub_ctx["max_rows"] = serde_json::json!(max);
            }
            if let Some(min) = sf.min_rows {
                sub_ctx["min_rows"] = serde_json::json!(min);
            }
            if sf.admin.init_collapsed {
                sub_ctx["init_collapsed"] = serde_json::json!(true);
            }
            if let Some(ref ls) = sf.admin.labels_singular {
                sub_ctx["add_label"] = serde_json::json!(ls.resolve_default());
            }
        }
        FieldType::Group => {
            // Nested group: sub-fields are stored as keys in the same row object
            let group_obj = match raw_value {
                Some(serde_json::Value::Object(_)) => raw_value,
                _ => None,
            };
            let nested_sub_fields: Vec<_> = sf.fields.iter().map(|nested_sf| {
                let nested_raw = group_obj
                    .and_then(|v| v.as_object())
                    .and_then(|m| m.get(&nested_sf.name));
                let nested_name = format!("{}[{}]", indexed_name, nested_sf.name);
                let nested_val = nested_raw
                    .map(|v| match v {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Null => String::new(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default();
                let nested_label = nested_sf.admin.label.as_ref()
                    .map(|ls| ls.resolve_default().to_string())
                    .unwrap_or_else(|| auto_label_from_name(&nested_sf.name));
                let mut nested_ctx = serde_json::json!({
                    "name": nested_name,
                    "field_type": nested_sf.field_type.as_str(),
                    "label": nested_label,
                    "value": nested_val,
                    "required": nested_sf.required,
                    "readonly": nested_sf.admin.readonly || locale_locked,
                    "locale_locked": locale_locked,
                    "placeholder": nested_sf.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
                    "description": nested_sf.admin.description.as_ref().map(|ls| ls.resolve_default()),
                });
                apply_field_type_extras(
                    nested_sf, &nested_val, &mut nested_ctx,
                    &HashMap::new(), &HashMap::new(), &nested_name,
                    non_default_locale, depth + 1,
                );
                nested_ctx
            }).collect();
            sub_ctx["sub_fields"] = serde_json::json!(nested_sub_fields);
            if sf.admin.collapsed {
                sub_ctx["collapsed"] = serde_json::json!(true);
            }
        }
        FieldType::Row | FieldType::Collapsible => {
            // Nested row/collapsible: sub-fields are stored as keys in the same row object
            let row_obj = match raw_value {
                Some(serde_json::Value::Object(_)) => raw_value,
                _ => None,
            };
            let nested_sub_fields: Vec<_> = sf.fields.iter().map(|nested_sf| {
                let nested_raw = row_obj
                    .and_then(|v| v.as_object())
                    .and_then(|m| m.get(&nested_sf.name));
                let nested_name = format!("{}[{}]", indexed_name, nested_sf.name);
                let nested_val = nested_raw
                    .map(|v| match v {
                        serde_json::Value::String(s) => s.clone(),
                        serde_json::Value::Null => String::new(),
                        other => other.to_string(),
                    })
                    .unwrap_or_default();
                let nested_label = nested_sf.admin.label.as_ref()
                    .map(|ls| ls.resolve_default().to_string())
                    .unwrap_or_else(|| auto_label_from_name(&nested_sf.name));
                let mut nested_ctx = serde_json::json!({
                    "name": nested_name,
                    "field_type": nested_sf.field_type.as_str(),
                    "label": nested_label,
                    "value": nested_val,
                    "required": nested_sf.required,
                    "readonly": nested_sf.admin.readonly || locale_locked,
                    "locale_locked": locale_locked,
                    "placeholder": nested_sf.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
                    "description": nested_sf.admin.description.as_ref().map(|ls| ls.resolve_default()),
                });
                apply_field_type_extras(
                    nested_sf, &nested_val, &mut nested_ctx,
                    &HashMap::new(), &HashMap::new(), &nested_name,
                    non_default_locale, depth + 1,
                );
                nested_ctx
            }).collect();
            sub_ctx["sub_fields"] = serde_json::json!(nested_sub_fields);
            if sf.field_type == FieldType::Collapsible && sf.admin.collapsed {
                sub_ctx["collapsed"] = serde_json::json!(true);
            }
        }
        FieldType::Tabs => {
            // Nested tabs: iterate tabs, each with sub-fields from the row object
            let row_obj = match raw_value {
                Some(serde_json::Value::Object(_)) => raw_value,
                _ => None,
            };
            let tabs_ctx: Vec<_> = sf.tabs.iter().map(|tab| {
                let tab_sub_fields: Vec<_> = tab.fields.iter().map(|nested_sf| {
                    let nested_raw = row_obj
                        .and_then(|v| v.as_object())
                        .and_then(|m| m.get(&nested_sf.name));
                    let nested_name = format!("{}[{}]", indexed_name, nested_sf.name);
                    let nested_val = nested_raw
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            serde_json::Value::Null => String::new(),
                            other => other.to_string(),
                        })
                        .unwrap_or_default();
                    let nested_label = nested_sf.admin.label.as_ref()
                        .map(|ls| ls.resolve_default().to_string())
                        .unwrap_or_else(|| auto_label_from_name(&nested_sf.name));
                    let mut nested_ctx = serde_json::json!({
                        "name": nested_name,
                        "field_type": nested_sf.field_type.as_str(),
                        "label": nested_label,
                        "value": nested_val,
                        "required": nested_sf.required,
                        "readonly": nested_sf.admin.readonly || locale_locked,
                        "locale_locked": locale_locked,
                        "placeholder": nested_sf.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
                        "description": nested_sf.admin.description.as_ref().map(|ls| ls.resolve_default()),
                    });
                    apply_field_type_extras(
                        nested_sf, &nested_val, &mut nested_ctx,
                        &HashMap::new(), &HashMap::new(), &nested_name,
                        non_default_locale, depth + 1,
                    );
                    nested_ctx
                }).collect();
                let mut tab_ctx = serde_json::json!({
                    "label": &tab.label,
                    "sub_fields": tab_sub_fields,
                });
                if let Some(ref desc) = tab.description {
                    tab_ctx["description"] = serde_json::json!(desc);
                }
                tab_ctx
            }).collect();
            sub_ctx["tabs"] = serde_json::json!(tabs_ctx);
        }
        _ => {}
    }

    sub_ctx
}

/// Enrich field contexts with data that requires DB access:
/// - Relationship fields: fetch available options from related collection
/// - Array fields: populate existing rows from hydrated document data
/// - Upload fields: fetch upload collection options with thumbnails
/// - Blocks fields: populate block rows from hydrated document data
pub(super) fn enrich_field_contexts(
    fields: &mut [serde_json::Value],
    field_defs: &[crate::core::field::FieldDefinition],
    doc_fields: &HashMap<String, serde_json::Value>,
    state: &crate::admin::AdminState,
    filter_hidden: bool,
    non_default_locale: bool,
    errors: &HashMap<String, String>,
) {
    use crate::core::upload;
    use crate::db::query::{self, LocaleContext};

    let reg = match state.registry.read() {
        Ok(r) => r,
        Err(_) => return,
    };
    let conn = match state.pool.get() {
        Ok(c) => c,
        Err(_) => return,
    };

    let rel_locale_ctx = LocaleContext::from_locale_string(None, &state.config.locale);

    let defs_iter: Box<dyn Iterator<Item = &crate::core::field::FieldDefinition>> = if filter_hidden {
        Box::new(field_defs.iter().filter(|f| !f.admin.hidden))
    } else {
        Box::new(field_defs.iter())
    };

    for (ctx, field_def) in fields.iter_mut().zip(defs_iter) {
        match field_def.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field_def.relationship {
                    // Fetch documents from related collection for options
                    if let Some(related_def) = reg.get_collection(&rc.collection) {
                        let title_field = related_def.title_field().map(|s| s.to_string());
                        let find_query = query::FindQuery::default();
                        if let Ok(docs) = query::find(&conn, &rc.collection, related_def, &find_query, rel_locale_ctx.as_ref()) {
                            if rc.has_many {
                                // Get selected IDs from hydrated document
                                let selected_ids: std::collections::HashSet<String> = match doc_fields.get(&field_def.name) {
                                    Some(serde_json::Value::Array(arr)) => {
                                        arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                                    }
                                    _ => std::collections::HashSet::new(),
                                };
                                let options: Vec<_> = docs.iter().map(|doc| {
                                    let label = title_field.as_ref()
                                        .and_then(|f| doc.get_str(f))
                                        .unwrap_or(&doc.id);
                                    serde_json::json!({
                                        "value": doc.id,
                                        "label": label,
                                        "selected": selected_ids.contains(&doc.id),
                                    })
                                }).collect();
                                ctx["relationship_options"] = serde_json::json!(options);
                            } else {
                                // Has-one: current value from context
                                let current_value = ctx.get("value")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let options: Vec<_> = docs.iter().map(|doc| {
                                    let label = title_field.as_ref()
                                        .and_then(|f| doc.get_str(f))
                                        .unwrap_or(&doc.id);
                                    serde_json::json!({
                                        "value": doc.id,
                                        "label": label,
                                        "selected": doc.id == current_value,
                                    })
                                }).collect();
                                ctx["relationship_options"] = serde_json::json!(options);
                            }
                        }
                    }
                }
            }
            FieldType::Array => {
                // Populate rows from hydrated document data
                let locale_locked = non_default_locale && !field_def.localized;
                let rows: Vec<serde_json::Value> = match doc_fields.get(&field_def.name) {
                    Some(serde_json::Value::Array(arr)) => {
                        arr.iter().enumerate().map(|(idx, row)| {
                            let row_obj = row.as_object();
                            let sub_values: Vec<_> = field_def.fields.iter().map(|sf| {
                                let raw_value = row_obj.and_then(|m| m.get(&sf.name));
                                build_enriched_sub_field_context(
                                    sf, raw_value, &field_def.name, idx,
                                    locale_locked, non_default_locale, 1, errors,
                                )
                            }).collect();
                            let row_has_errors = sub_values.iter()
                                .any(|sf_ctx| sf_ctx.get("error").is_some());
                            let mut row_json = serde_json::json!({
                                "index": idx,
                                "sub_fields": sub_values,
                            });
                            if row_has_errors {
                                row_json["has_errors"] = serde_json::json!(true);
                            }
                            // Compute custom row label
                            if let Some(label) = compute_row_label(
                                &field_def.admin, None, row_obj, &state.hook_runner,
                            ) {
                                row_json["custom_label"] = serde_json::json!(label);
                            }
                            row_json
                        }).collect()
                    }
                    _ => Vec::new(),
                };
                ctx["row_count"] = serde_json::json!(rows.len());
                ctx["rows"] = serde_json::json!(rows);
                // Enrich Upload/Relationship sub-fields within each row
                if let Some(rows_arr) = ctx.get_mut("rows").and_then(|v| v.as_array_mut()) {
                    for row_ctx in rows_arr.iter_mut() {
                        if let Some(sub_arr) = row_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                            enrich_nested_fields(sub_arr, &field_def.fields, &conn, &reg, rel_locale_ctx.as_ref());
                        }
                    }
                }
            }
            FieldType::Upload => {
                // Upload is a has-one relationship to an upload collection
                if let Some(ref rc) = field_def.relationship {
                    if let Some(related_def) = reg.get_collection(&rc.collection) {
                        let title_field = related_def.title_field().map(|s| s.to_string());
                        let admin_thumbnail = related_def.upload.as_ref()
                            .and_then(|u| u.admin_thumbnail.as_ref().cloned());
                        let find_query = query::FindQuery::default();
                        if let Ok(mut docs) = query::find(&conn, &rc.collection, related_def, &find_query, rel_locale_ctx.as_ref()) {
                            // Assemble sizes for thumbnail lookup
                            if let Some(ref upload_config) = related_def.upload {
                                if upload_config.enabled {
                                    for doc in &mut docs {
                                        upload::assemble_sizes_object(doc, upload_config);
                                    }
                                }
                            }

                            let current_value = ctx.get("value")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();

                            let mut selected_preview_url = None;
                            let mut selected_filename = None;

                            let options: Vec<_> = docs.iter().map(|doc| {
                                let label = doc.get_str("filename")
                                    .or_else(|| title_field.as_ref().and_then(|f| doc.get_str(f)))
                                    .unwrap_or(&doc.id);
                                let mime = doc.get_str("mime_type").unwrap_or("");
                                let is_image = mime.starts_with("image/");

                                // Get thumbnail URL
                                let thumb_url = if is_image {
                                    admin_thumbnail.as_ref()
                                        .and_then(|thumb_name| {
                                            doc.fields.get("sizes")
                                                .and_then(|v| v.get(thumb_name))
                                                .and_then(|v| v.get("url"))
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                        })
                                        .or_else(|| doc.get_str("url").map(|s| s.to_string()))
                                } else {
                                    None
                                };

                                let is_selected = doc.id == current_value;
                                if is_selected {
                                    selected_preview_url = thumb_url.clone();
                                    selected_filename = Some(label.to_string());
                                }

                                let mut opt = serde_json::json!({
                                    "value": doc.id,
                                    "label": label,
                                    "selected": is_selected,
                                });
                                if let Some(ref url) = thumb_url {
                                    opt["thumbnail_url"] = serde_json::json!(url);
                                }
                                if is_image {
                                    opt["is_image"] = serde_json::json!(true);
                                }
                                opt["filename"] = serde_json::json!(label);
                                opt
                            }).collect();
                            ctx["relationship_options"] = serde_json::json!(options);
                            ctx["relationship_collection"] = serde_json::json!(rc.collection);

                            if let Some(url) = selected_preview_url {
                                ctx["selected_preview_url"] = serde_json::json!(url);
                            }
                            if let Some(fname) = selected_filename {
                                ctx["selected_filename"] = serde_json::json!(fname);
                            }
                        }
                    }
                }
            }
            FieldType::Blocks => {
                // Populate rows from hydrated document data
                let locale_locked = non_default_locale && !field_def.localized;
                let rows: Vec<serde_json::Value> = match doc_fields.get(&field_def.name) {
                    Some(serde_json::Value::Array(arr)) => {
                        arr.iter().enumerate().map(|(idx, row)| {
                            let row_obj = row.as_object();
                            let block_type = row_obj
                                .and_then(|m| m.get("_block_type"))
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            let block_label = field_def.blocks.iter()
                                .find(|bd| bd.block_type == block_type)
                                .and_then(|bd| bd.label.as_ref().map(|ls| ls.resolve_default()))
                                .unwrap_or(block_type);
                            let block_def = field_def.blocks.iter()
                                .find(|bd| bd.block_type == block_type);
                            let block_label_field = block_def.and_then(|bd| bd.label_field.as_deref());
                            let sub_values: Vec<_> = block_def
                                .map(|bd| bd.fields.iter().map(|sf| {
                                    let raw_value = row_obj.and_then(|m| m.get(&sf.name));
                                    build_enriched_sub_field_context(
                                        sf, raw_value, &field_def.name, idx,
                                        locale_locked, non_default_locale, 1, errors,
                                    )
                                }).collect())
                                .unwrap_or_default();
                            let row_has_errors = sub_values.iter()
                                .any(|sf_ctx| sf_ctx.get("error").is_some());
                            let mut row_json = serde_json::json!({
                                "index": idx,
                                "_block_type": block_type,
                                "block_label": block_label,
                                "sub_fields": sub_values,
                            });
                            if row_has_errors {
                                row_json["has_errors"] = serde_json::json!(true);
                            }
                            // Compute custom row label
                            if let Some(label) = compute_row_label(
                                &field_def.admin, block_label_field, row_obj, &state.hook_runner,
                            ) {
                                row_json["custom_label"] = serde_json::json!(label);
                            }
                            row_json
                        }).collect()
                    }
                    _ => Vec::new(),
                };
                ctx["row_count"] = serde_json::json!(rows.len());
                ctx["rows"] = serde_json::json!(rows);
                // Enrich Upload/Relationship sub-fields within each block row
                if let Some(rows_arr) = ctx.get_mut("rows").and_then(|v| v.as_array_mut()) {
                    for row_ctx in rows_arr.iter_mut() {
                        let block_type = row_ctx.get("_block_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if let Some(block_def) = field_def.blocks.iter().find(|bd| bd.block_type == block_type) {
                            if let Some(sub_arr) = row_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                                enrich_nested_fields(sub_arr, &block_def.fields, &conn, &reg, rel_locale_ctx.as_ref());
                            }
                        }
                    }
                }
            }
            FieldType::Row | FieldType::Collapsible | FieldType::Group => {
                // Recurse into layout/group sub-fields to enrich nested Upload/Relationship fields
                if let Some(sub_arr) = ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                    enrich_nested_fields(sub_arr, &field_def.fields, &conn, &reg, rel_locale_ctx.as_ref());
                }
            }
            FieldType::Tabs => {
                // Recurse into each tab's sub-fields
                if let Some(tabs_arr) = ctx.get_mut("tabs").and_then(|v| v.as_array_mut()) {
                    for (tab_ctx, tab_def) in tabs_arr.iter_mut().zip(field_def.tabs.iter()) {
                        if let Some(sub_arr) = tab_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                            enrich_nested_fields(sub_arr, &tab_def.fields, &conn, &reg, rel_locale_ctx.as_ref());
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// Recursively enrich Upload and Relationship sub-field contexts with options from the database.
/// Called for sub-fields inside layout containers (Row, Collapsible, Tabs, Group) and
/// composite fields (Array, Blocks) that can't be enriched during initial context building.
fn enrich_nested_fields(
    sub_fields: &mut Vec<serde_json::Value>,
    field_defs: &[crate::core::field::FieldDefinition],
    conn: &rusqlite::Connection,
    reg: &crate::core::Registry,
    rel_locale_ctx: Option<&crate::db::query::LocaleContext>,
) {
    use crate::core::field::FieldType;
    use crate::core::upload;
    use crate::db::query;

    for (ctx, field_def) in sub_fields.iter_mut().zip(field_defs.iter()) {
        match field_def.field_type {
            FieldType::Relationship => {
                if let Some(ref rc) = field_def.relationship {
                    if let Some(related_def) = reg.get_collection(&rc.collection) {
                        let title_field = related_def.title_field().map(|s| s.to_string());
                        let find_query = query::FindQuery::default();
                        if let Ok(docs) = query::find(conn, &rc.collection, related_def, &find_query, rel_locale_ctx) {
                            let current_value = ctx.get("value")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            let options: Vec<_> = docs.iter().map(|doc| {
                                let label = title_field.as_ref()
                                    .and_then(|f| doc.get_str(f))
                                    .unwrap_or(&doc.id);
                                serde_json::json!({
                                    "value": doc.id,
                                    "label": label,
                                    "selected": doc.id == current_value,
                                })
                            }).collect();
                            ctx["relationship_options"] = serde_json::json!(options);
                        }
                    }
                }
            }
            FieldType::Upload => {
                if let Some(ref rc) = field_def.relationship {
                    if let Some(related_def) = reg.get_collection(&rc.collection) {
                        let title_field = related_def.title_field().map(|s| s.to_string());
                        let admin_thumbnail = related_def.upload.as_ref()
                            .and_then(|u| u.admin_thumbnail.as_ref().cloned());
                        let find_query = query::FindQuery::default();
                        if let Ok(mut docs) = query::find(conn, &rc.collection, related_def, &find_query, rel_locale_ctx) {
                            if let Some(ref upload_config) = related_def.upload {
                                if upload_config.enabled {
                                    for doc in &mut docs {
                                        upload::assemble_sizes_object(doc, upload_config);
                                    }
                                }
                            }

                            let current_value = ctx.get("value")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();

                            let mut selected_preview_url = None;
                            let mut selected_filename = None;

                            let options: Vec<_> = docs.iter().map(|doc| {
                                let label = doc.get_str("filename")
                                    .or_else(|| title_field.as_ref().and_then(|f| doc.get_str(f)))
                                    .unwrap_or(&doc.id);
                                let mime = doc.get_str("mime_type").unwrap_or("");
                                let is_image = mime.starts_with("image/");

                                let thumb_url = if is_image {
                                    admin_thumbnail.as_ref()
                                        .and_then(|thumb_name| {
                                            doc.fields.get("sizes")
                                                .and_then(|v| v.get(thumb_name))
                                                .and_then(|v| v.get("url"))
                                                .and_then(|v| v.as_str())
                                                .map(|s| s.to_string())
                                        })
                                        .or_else(|| doc.get_str("url").map(|s| s.to_string()))
                                } else {
                                    None
                                };

                                let is_selected = doc.id == current_value;
                                if is_selected {
                                    selected_preview_url = thumb_url.clone();
                                    selected_filename = Some(label.to_string());
                                }

                                let mut opt = serde_json::json!({
                                    "value": doc.id,
                                    "label": label,
                                    "selected": is_selected,
                                });
                                if let Some(ref url) = thumb_url {
                                    opt["thumbnail_url"] = serde_json::json!(url);
                                }
                                if is_image {
                                    opt["is_image"] = serde_json::json!(true);
                                }
                                opt["filename"] = serde_json::json!(label);
                                opt
                            }).collect();
                            ctx["relationship_options"] = serde_json::json!(options);
                            ctx["relationship_collection"] = serde_json::json!(rc.collection);

                            if let Some(url) = selected_preview_url {
                                ctx["selected_preview_url"] = serde_json::json!(url);
                            }
                            if let Some(fname) = selected_filename {
                                ctx["selected_filename"] = serde_json::json!(fname);
                            }
                        }
                    }
                }
            }
            FieldType::Row | FieldType::Collapsible | FieldType::Group => {
                if let Some(sub_arr) = ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                    enrich_nested_fields(sub_arr, &field_def.fields, conn, reg, rel_locale_ctx);
                }
            }
            FieldType::Tabs => {
                if let Some(tabs_arr) = ctx.get_mut("tabs").and_then(|v| v.as_array_mut()) {
                    for (tab_ctx, tab_def) in tabs_arr.iter_mut().zip(field_def.tabs.iter()) {
                        if let Some(sub_arr) = tab_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                            enrich_nested_fields(sub_arr, &tab_def.fields, conn, reg, rel_locale_ctx);
                        }
                    }
                }
            }
            FieldType::Array => {
                // Recurse into array rows' sub-fields
                if let Some(rows_arr) = ctx.get_mut("rows").and_then(|v| v.as_array_mut()) {
                    for row_ctx in rows_arr.iter_mut() {
                        if let Some(sub_arr) = row_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                            enrich_nested_fields(sub_arr, &field_def.fields, conn, reg, rel_locale_ctx);
                        }
                    }
                }
            }
            FieldType::Blocks => {
                // Recurse into block rows' sub-fields, matching each row's block type
                if let Some(rows_arr) = ctx.get_mut("rows").and_then(|v| v.as_array_mut()) {
                    for row_ctx in rows_arr.iter_mut() {
                        let block_type = row_ctx.get("_block_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if let Some(block_def) = field_def.blocks.iter().find(|bd| bd.block_type == block_type) {
                            if let Some(sub_arr) = row_ctx.get_mut("sub_fields").and_then(|v| v.as_array_mut()) {
                                enrich_nested_fields(sub_arr, &block_def.fields, conn, reg, rel_locale_ctx);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::field::{FieldDefinition, SelectOption, LocalizedString, BlockDefinition};

    fn make_field(name: &str, ft: FieldType) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: ft,
            ..Default::default()
        }
    }

    // --- build_field_contexts: array/block sub-field enrichment tests ---

    #[test]
    fn build_field_contexts_array_sub_fields_include_type_and_label() {
        let mut arr_field = make_field("items", FieldType::Array);
        arr_field.fields = vec![
            make_field("title", FieldType::Text),
            make_field("body", FieldType::Richtext),
        ];
        let fields = vec![arr_field];
        let values = HashMap::new();
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        assert_eq!(result.len(), 1);
        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields.len(), 2);
        assert_eq!(sub_fields[0]["field_type"], "text");
        assert_eq!(sub_fields[0]["label"], "Title");
        assert_eq!(sub_fields[1]["field_type"], "richtext");
        assert_eq!(sub_fields[1]["label"], "Body");
    }

    #[test]
    fn build_field_contexts_array_select_sub_field_includes_options() {
        let mut select_sf = make_field("status", FieldType::Select);
        select_sf.options = vec![
            SelectOption { label: LocalizedString::Plain("Draft".to_string()), value: "draft".to_string() },
            SelectOption { label: LocalizedString::Plain("Published".to_string()), value: "published".to_string() },
        ];
        let mut arr_field = make_field("items", FieldType::Array);
        arr_field.fields = vec![select_sf];
        let fields = vec![arr_field];
        let values = HashMap::new();
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        let opts = sub_fields[0]["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["value"], "draft");
        assert_eq!(opts[1]["value"], "published");
    }

    #[test]
    fn build_field_contexts_blocks_sub_fields_include_type_and_label() {
        let mut blocks_field = make_field("content", FieldType::Blocks);
        blocks_field.blocks = vec![BlockDefinition {
            block_type: "rich".to_string(),
            label: Some(LocalizedString::Plain("Rich Text".to_string())),
            fields: vec![
                make_field("heading", FieldType::Text),
                make_field("body", FieldType::Richtext),
            ],
            ..Default::default()
        }];
        let fields = vec![blocks_field];
        let values = HashMap::new();
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        let block_defs = result[0]["block_definitions"].as_array().unwrap();
        assert_eq!(block_defs.len(), 1);
        let block_fields = block_defs[0]["fields"].as_array().unwrap();
        assert_eq!(block_fields.len(), 2);
        assert_eq!(block_fields[0]["field_type"], "text");
        assert_eq!(block_fields[0]["label"], "Heading");
        assert_eq!(block_fields[1]["field_type"], "richtext");
        assert_eq!(block_fields[1]["label"], "Body");
    }

    #[test]
    fn build_field_contexts_blocks_select_sub_field_includes_options() {
        let mut select_sf = make_field("align", FieldType::Select);
        select_sf.options = vec![
            SelectOption { label: LocalizedString::Plain("Left".to_string()), value: "left".to_string() },
            SelectOption { label: LocalizedString::Plain("Center".to_string()), value: "center".to_string() },
        ];
        let mut blocks_field = make_field("layout", FieldType::Blocks);
        blocks_field.blocks = vec![BlockDefinition {
            block_type: "section".to_string(),
            label: None,
            fields: vec![select_sf],
            ..Default::default()
        }];
        let fields = vec![blocks_field];
        let values = HashMap::new();
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        let block_defs = result[0]["block_definitions"].as_array().unwrap();
        let block_fields = block_defs[0]["fields"].as_array().unwrap();
        let opts = block_fields[0]["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["value"], "left");
        assert_eq!(opts[1]["value"], "center");
    }

    // --- build_field_contexts: date field tests ---

    #[test]
    fn build_field_contexts_date_default_day_only() {
        let date_field = make_field("published_at", FieldType::Date);
        let fields = vec![date_field];
        let mut values = HashMap::new();
        values.insert("published_at".to_string(), "2026-01-15T12:00:00.000Z".to_string());
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        assert_eq!(result[0]["picker_appearance"], "dayOnly");
        assert_eq!(result[0]["date_only_value"], "2026-01-15");
    }

    #[test]
    fn build_field_contexts_date_day_and_time() {
        let mut date_field = make_field("event_at", FieldType::Date);
        date_field.picker_appearance = Some("dayAndTime".to_string());
        let fields = vec![date_field];
        let mut values = HashMap::new();
        values.insert("event_at".to_string(), "2026-01-15T09:30:00.000Z".to_string());
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        assert_eq!(result[0]["picker_appearance"], "dayAndTime");
        assert_eq!(result[0]["datetime_local_value"], "2026-01-15T09:30");
    }

    #[test]
    fn build_field_contexts_date_time_only() {
        let mut date_field = make_field("reminder", FieldType::Date);
        date_field.picker_appearance = Some("timeOnly".to_string());
        let fields = vec![date_field];
        let mut values = HashMap::new();
        values.insert("reminder".to_string(), "14:30".to_string());
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        assert_eq!(result[0]["picker_appearance"], "timeOnly");
        assert_eq!(result[0]["value"], "14:30");
    }

    #[test]
    fn build_field_contexts_date_month_only() {
        let mut date_field = make_field("birth_month", FieldType::Date);
        date_field.picker_appearance = Some("monthOnly".to_string());
        let fields = vec![date_field];
        let mut values = HashMap::new();
        values.insert("birth_month".to_string(), "2026-01".to_string());
        let errors = HashMap::new();
        let result = build_field_contexts(&fields, &values, &errors, false, false);
        assert_eq!(result[0]["picker_appearance"], "monthOnly");
        assert_eq!(result[0]["value"], "2026-01");
    }

    // --- safe_template_id tests ---

    #[test]
    fn safe_template_id_simple_name() {
        assert_eq!(safe_template_id("items"), "items");
    }

    #[test]
    fn safe_template_id_with_brackets() {
        assert_eq!(safe_template_id("content[0][items]"), "content-0-items");
    }

    #[test]
    fn safe_template_id_nested_index_placeholder() {
        assert_eq!(safe_template_id("content[__INDEX__][items]"), "content-__INDEX__-items");
    }

    // --- Recursive build_field_contexts tests (nested composites) ---

    #[test]
    fn build_field_contexts_array_has_template_id() {
        let mut arr_field = make_field("items", FieldType::Array);
        arr_field.fields = vec![make_field("title", FieldType::Text)];
        let fields = vec![arr_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["template_id"], "items");
    }

    #[test]
    fn build_field_contexts_blocks_has_template_id() {
        let mut blocks_field = make_field("content", FieldType::Blocks);
        blocks_field.blocks = vec![BlockDefinition {
            block_type: "text".to_string(),
            label: None,
            fields: vec![make_field("body", FieldType::Text)],
            ..Default::default()
        }];
        let fields = vec![blocks_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["template_id"], "content");
    }

    #[test]
    fn build_field_contexts_array_sub_fields_have_indexed_names() {
        let mut arr_field = make_field("slides", FieldType::Array);
        arr_field.fields = vec![
            make_field("title", FieldType::Text),
            make_field("body", FieldType::Textarea),
        ];
        let fields = vec![arr_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        // Sub-fields in the template context should have __INDEX__ placeholder names
        assert_eq!(sub_fields[0]["name"], "slides[__INDEX__][title]");
        assert_eq!(sub_fields[1]["name"], "slides[__INDEX__][body]");
    }

    #[test]
    fn build_field_contexts_nested_array_in_blocks() {
        // blocks field with a block that contains an array sub-field
        let mut inner_array = make_field("images", FieldType::Array);
        inner_array.fields = vec![
            make_field("url", FieldType::Text),
            make_field("caption", FieldType::Text),
        ];
        let mut blocks_field = make_field("content", FieldType::Blocks);
        blocks_field.blocks = vec![BlockDefinition {
            block_type: "gallery".to_string(),
            label: Some(LocalizedString::Plain("Gallery".to_string())),
            fields: vec![
                make_field("title", FieldType::Text),
                inner_array,
            ],
            ..Default::default()
        }];
        let fields = vec![blocks_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

        let block_defs = result[0]["block_definitions"].as_array().unwrap();
        assert_eq!(block_defs.len(), 1);
        let block_fields = block_defs[0]["fields"].as_array().unwrap();
        assert_eq!(block_fields.len(), 2);

        // First field is simple text
        assert_eq!(block_fields[0]["field_type"], "text");
        assert_eq!(block_fields[0]["name"], "content[__INDEX__][title]");

        // Second field is a nested array
        assert_eq!(block_fields[1]["field_type"], "array");
        assert_eq!(block_fields[1]["name"], "content[__INDEX__][images]");

        // The nested array should have its own sub_fields with double __INDEX__
        let nested_sub_fields = block_fields[1]["sub_fields"].as_array().unwrap();
        assert_eq!(nested_sub_fields.len(), 2);
        assert_eq!(nested_sub_fields[0]["name"], "content[__INDEX__][images][__INDEX__][url]");
        assert_eq!(nested_sub_fields[1]["name"], "content[__INDEX__][images][__INDEX__][caption]");

        // Nested array should have template_id
        assert!(block_fields[1]["template_id"].as_str().is_some());
    }

    #[test]
    fn build_field_contexts_nested_blocks_in_array() {
        // array field with a blocks sub-field
        let mut inner_blocks = make_field("sections", FieldType::Blocks);
        inner_blocks.blocks = vec![BlockDefinition {
            block_type: "text".to_string(),
            label: None,
            fields: vec![make_field("body", FieldType::Richtext)],
            ..Default::default()
        }];
        let mut arr_field = make_field("pages", FieldType::Array);
        arr_field.fields = vec![
            make_field("title", FieldType::Text),
            inner_blocks,
        ];
        let fields = vec![arr_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields.len(), 2);
        assert_eq!(sub_fields[0]["field_type"], "text");
        assert_eq!(sub_fields[1]["field_type"], "blocks");

        // Nested blocks should have block_definitions
        let nested_block_defs = sub_fields[1]["block_definitions"].as_array().unwrap();
        assert_eq!(nested_block_defs.len(), 1);
        assert_eq!(nested_block_defs[0]["block_type"], "text");

        // The nested block's fields should have proper names
        let nested_block_fields = nested_block_defs[0]["fields"].as_array().unwrap();
        assert_eq!(nested_block_fields[0]["field_type"], "richtext");
        assert_eq!(nested_block_fields[0]["name"], "pages[__INDEX__][sections][__INDEX__][body]");
    }

    #[test]
    fn build_field_contexts_nested_group_in_array() {
        // array with a group sub-field
        let mut inner_group = make_field("meta", FieldType::Group);
        inner_group.fields = vec![
            make_field("author", FieldType::Text),
            make_field("date", FieldType::Date),
        ];
        let mut arr_field = make_field("entries", FieldType::Array);
        arr_field.fields = vec![inner_group];
        let fields = vec![arr_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields.len(), 1);
        assert_eq!(sub_fields[0]["field_type"], "group");

        // Group sub-fields inside array use bracketed naming
        let group_sub_fields = sub_fields[0]["sub_fields"].as_array().unwrap();
        assert_eq!(group_sub_fields.len(), 2);
        assert_eq!(group_sub_fields[0]["name"], "entries[__INDEX__][meta][author]");
        assert_eq!(group_sub_fields[1]["name"], "entries[__INDEX__][meta][date]");
    }

    #[test]
    fn build_field_contexts_nested_array_in_array() {
        // array containing an array sub-field
        let mut inner_array = make_field("tags", FieldType::Array);
        inner_array.fields = vec![make_field("name", FieldType::Text)];
        let mut outer_array = make_field("items", FieldType::Array);
        outer_array.fields = vec![
            make_field("title", FieldType::Text),
            inner_array,
        ];
        let fields = vec![outer_array];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);

        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields[1]["field_type"], "array");

        // Nested array sub_fields have double __INDEX__
        let nested_sub = sub_fields[1]["sub_fields"].as_array().unwrap();
        assert_eq!(nested_sub[0]["name"], "items[__INDEX__][tags][__INDEX__][name]");
    }

    // --- Recursive enrichment tests (build_enriched_sub_field_context) ---

    #[test]
    fn enriched_sub_field_nested_array_populates_rows() {
        let mut inner_array = make_field("images", FieldType::Array);
        inner_array.fields = vec![
            make_field("url", FieldType::Text),
            make_field("alt", FieldType::Text),
        ];

        // Simulate hydrated data: an array with 2 rows
        let raw_value = serde_json::json!([
            {"url": "img1.jpg", "alt": "First"},
            {"url": "img2.jpg", "alt": "Second"},
        ]);

        let ctx = build_enriched_sub_field_context(
            &inner_array, Some(&raw_value), "content", 0,
            false, false, 1, &HashMap::new(),
        );

        assert_eq!(ctx["field_type"], "array");
        assert_eq!(ctx["row_count"], 2);

        let rows = ctx["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 2);

        // First row sub_fields
        let row0_fields = rows[0]["sub_fields"].as_array().unwrap();
        assert_eq!(row0_fields[0]["name"], "content[0][images][0][url]");
        assert_eq!(row0_fields[0]["value"], "img1.jpg");
        assert_eq!(row0_fields[1]["name"], "content[0][images][0][alt]");
        assert_eq!(row0_fields[1]["value"], "First");

        // Second row sub_fields
        let row1_fields = rows[1]["sub_fields"].as_array().unwrap();
        assert_eq!(row1_fields[0]["value"], "img2.jpg");
        assert_eq!(row1_fields[1]["value"], "Second");

        // Template sub_fields should use __INDEX__
        let template_sub = ctx["sub_fields"].as_array().unwrap();
        assert_eq!(template_sub[0]["name"], "content[0][images][__INDEX__][url]");
    }

    #[test]
    fn enriched_sub_field_nested_blocks_populates_rows() {
        let mut inner_blocks = make_field("sections", FieldType::Blocks);
        inner_blocks.blocks = vec![BlockDefinition {
            block_type: "text".to_string(),
            label: Some(LocalizedString::Plain("Text".to_string())),
            fields: vec![make_field("body", FieldType::Richtext)],
            ..Default::default()
        }];

        let raw_value = serde_json::json!([
            {"_block_type": "text", "body": "<p>Hello</p>"},
        ]);

        let ctx = build_enriched_sub_field_context(
            &inner_blocks, Some(&raw_value), "page", 2,
            false, false, 1, &HashMap::new(),
        );

        assert_eq!(ctx["field_type"], "blocks");
        assert_eq!(ctx["row_count"], 1);

        let rows = ctx["rows"].as_array().unwrap();
        assert_eq!(rows[0]["_block_type"], "text");
        assert_eq!(rows[0]["block_label"], "Text");

        let sub_fields = rows[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields[0]["name"], "page[2][sections][0][body]");
        assert_eq!(sub_fields[0]["value"], "<p>Hello</p>");

        // Block definitions for templates
        let block_defs = ctx["block_definitions"].as_array().unwrap();
        assert_eq!(block_defs.len(), 1);
    }

    #[test]
    fn enriched_sub_field_nested_group_populates_values() {
        let mut inner_group = make_field("meta", FieldType::Group);
        inner_group.fields = vec![
            make_field("author", FieldType::Text),
            make_field("published", FieldType::Checkbox),
        ];

        let raw_value = serde_json::json!({
            "author": "Alice",
            "published": "1",
        });

        let ctx = build_enriched_sub_field_context(
            &inner_group, Some(&raw_value), "items", 0,
            false, false, 1, &HashMap::new(),
        );

        assert_eq!(ctx["field_type"], "group");
        let sub_fields = ctx["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields.len(), 2);
        assert_eq!(sub_fields[0]["name"], "items[0][meta][author]");
        assert_eq!(sub_fields[0]["value"], "Alice");
        assert_eq!(sub_fields[1]["name"], "items[0][meta][published]");
        assert_eq!(sub_fields[1]["checked"], true);
    }

    #[test]
    fn enriched_sub_field_empty_nested_array() {
        let mut inner_array = make_field("tags", FieldType::Array);
        inner_array.fields = vec![make_field("name", FieldType::Text)];

        // No data
        let ctx = build_enriched_sub_field_context(
            &inner_array, None, "items", 0,
            false, false, 1, &HashMap::new(),
        );

        assert_eq!(ctx["field_type"], "array");
        assert_eq!(ctx["row_count"], 0);
        let rows = ctx["rows"].as_array().unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn enriched_sub_field_select_preserves_selected() {
        let mut select_field = make_field("status", FieldType::Select);
        select_field.options = vec![
            SelectOption { label: LocalizedString::Plain("Draft".to_string()), value: "draft".to_string() },
            SelectOption { label: LocalizedString::Plain("Published".to_string()), value: "published".to_string() },
        ];

        let raw_value = serde_json::json!("published");

        let ctx = build_enriched_sub_field_context(
            &select_field, Some(&raw_value), "items", 0,
            false, false, 1, &HashMap::new(),
        );

        let opts = ctx["options"].as_array().unwrap();
        assert_eq!(opts[0]["selected"], false);
        assert_eq!(opts[1]["selected"], true);
    }

    #[test]
    fn max_depth_prevents_infinite_recursion() {
        // Build a deeply nested array structure
        fn make_nested_array(depth: usize) -> FieldDefinition {
            let mut field = make_field(&format!("level{}", depth), FieldType::Array);
            if depth < 10 {
                field.fields = vec![make_nested_array(depth + 1)];
            } else {
                field.fields = vec![make_field("leaf", FieldType::Text)];
            }
            field
        }
        let deep = make_nested_array(0);
        let fields = vec![deep];
        // This should not stack overflow -- MAX_FIELD_DEPTH caps recursion
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0]["field_type"], "array");
    }

    // --- split_sidebar_fields tests ---

    #[test]
    fn split_sidebar_fields_separates_by_position() {
        let fields = vec![
            serde_json::json!({"name": "title", "field_type": "text"}),
            serde_json::json!({"name": "slug", "field_type": "text", "position": "sidebar"}),
            serde_json::json!({"name": "body", "field_type": "richtext"}),
            serde_json::json!({"name": "status", "field_type": "select", "position": "sidebar"}),
        ];
        let (main, sidebar) = split_sidebar_fields(fields);
        assert_eq!(main.len(), 2);
        assert_eq!(sidebar.len(), 2);
        assert_eq!(main[0]["name"], "title");
        assert_eq!(main[1]["name"], "body");
        assert_eq!(sidebar[0]["name"], "slug");
        assert_eq!(sidebar[1]["name"], "status");
    }

    #[test]
    fn split_sidebar_fields_no_sidebar() {
        let fields = vec![
            serde_json::json!({"name": "title", "field_type": "text"}),
            serde_json::json!({"name": "body", "field_type": "richtext"}),
        ];
        let (main, sidebar) = split_sidebar_fields(fields);
        assert_eq!(main.len(), 2);
        assert!(sidebar.is_empty());
    }

    #[test]
    fn split_sidebar_fields_all_sidebar() {
        let fields = vec![
            serde_json::json!({"name": "a", "position": "sidebar"}),
            serde_json::json!({"name": "b", "position": "sidebar"}),
        ];
        let (main, sidebar) = split_sidebar_fields(fields);
        assert!(main.is_empty());
        assert_eq!(sidebar.len(), 2);
    }

    #[test]
    fn split_sidebar_fields_empty() {
        let (main, sidebar) = split_sidebar_fields(vec![]);
        assert!(main.is_empty());
        assert!(sidebar.is_empty());
    }

    // --- build_field_contexts: filter_hidden tests ---

    #[test]
    fn build_field_contexts_filter_hidden_removes_hidden_fields() {
        let mut hidden_field = make_field("secret", FieldType::Text);
        hidden_field.admin.hidden = true;
        let fields = vec![
            make_field("title", FieldType::Text),
            hidden_field,
            make_field("body", FieldType::Textarea),
        ];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), true, false);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0]["name"], "title");
        assert_eq!(result[1]["name"], "body");
    }

    #[test]
    fn build_field_contexts_no_filter_includes_hidden_fields() {
        let mut hidden_field = make_field("secret", FieldType::Text);
        hidden_field.admin.hidden = true;
        let fields = vec![
            make_field("title", FieldType::Text),
            hidden_field,
        ];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result.len(), 2);
    }

    // --- build_field_contexts: relationship tests ---

    #[test]
    fn build_field_contexts_relationship_has_collection_info() {
        use crate::core::field::RelationshipConfig;
        let mut rel_field = make_field("author", FieldType::Relationship);
        rel_field.relationship = Some(RelationshipConfig {
            collection: "users".to_string(),
            has_many: false,
            max_depth: None,
        });
        let fields = vec![rel_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["relationship_collection"], "users");
        assert_eq!(result[0]["has_many"], false);
    }

    #[test]
    fn build_field_contexts_relationship_has_many() {
        use crate::core::field::RelationshipConfig;
        let mut rel_field = make_field("tags", FieldType::Relationship);
        rel_field.relationship = Some(RelationshipConfig {
            collection: "tags".to_string(),
            has_many: true,
            max_depth: None,
        });
        let fields = vec![rel_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["relationship_collection"], "tags");
        assert_eq!(result[0]["has_many"], true);
    }

    // --- build_field_contexts: checkbox tests ---

    #[test]
    fn build_field_contexts_checkbox_checked_values() {
        for val in &["1", "true", "on", "yes"] {
            let mut values = HashMap::new();
            values.insert("active".to_string(), val.to_string());
            let fields = vec![make_field("active", FieldType::Checkbox)];
            let result = build_field_contexts(&fields, &values, &HashMap::new(), false, false);
            assert_eq!(result[0]["checked"], true, "Checkbox should be checked for value '{}'", val);
        }
    }

    #[test]
    fn build_field_contexts_checkbox_unchecked_values() {
        for val in &["0", "false", "off", "no", ""] {
            let mut values = HashMap::new();
            values.insert("active".to_string(), val.to_string());
            let fields = vec![make_field("active", FieldType::Checkbox)];
            let result = build_field_contexts(&fields, &values, &HashMap::new(), false, false);
            assert_eq!(result[0]["checked"], false, "Checkbox should be unchecked for value '{}'", val);
        }
    }

    // --- build_field_contexts: upload field tests ---

    #[test]
    fn build_field_contexts_upload_has_collection() {
        use crate::core::field::RelationshipConfig;
        let mut upload_field = make_field("image", FieldType::Upload);
        upload_field.relationship = Some(RelationshipConfig {
            collection: "media".to_string(),
            has_many: false,
            max_depth: None,
        });
        let fields = vec![upload_field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["relationship_collection"], "media");
    }

    // --- build_field_contexts: select tests ---

    #[test]
    fn build_field_contexts_select_marks_selected_option() {
        let mut sel = make_field("color", FieldType::Select);
        sel.options = vec![
            SelectOption { label: LocalizedString::Plain("Red".to_string()), value: "red".to_string() },
            SelectOption { label: LocalizedString::Plain("Blue".to_string()), value: "blue".to_string() },
        ];
        let mut values = HashMap::new();
        values.insert("color".to_string(), "blue".to_string());
        let fields = vec![sel];
        let result = build_field_contexts(&fields, &values, &HashMap::new(), false, false);
        let opts = result[0]["options"].as_array().unwrap();
        assert_eq!(opts[0]["selected"], false);
        assert_eq!(opts[1]["selected"], true);
    }

    // --- build_field_contexts: error propagation ---

    #[test]
    fn build_field_contexts_errors_attached_to_fields() {
        let fields = vec![make_field("title", FieldType::Text)];
        let mut errors = HashMap::new();
        errors.insert("title".to_string(), "Title is required".to_string());
        let result = build_field_contexts(&fields, &HashMap::new(), &errors, false, false);
        assert_eq!(result[0]["error"], "Title is required");
    }

    #[test]
    fn build_field_contexts_no_error_when_field_valid() {
        let fields = vec![make_field("title", FieldType::Text)];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert!(result[0].get("error").is_none());
    }

    // --- build_field_contexts: locale locking ---

    #[test]
    fn build_field_contexts_locale_locked_non_localized_field() {
        let fields = vec![make_field("slug", FieldType::Text)]; // not localized
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, true);
        assert_eq!(result[0]["locale_locked"], true);
        assert_eq!(result[0]["readonly"], true);
    }

    #[test]
    fn build_field_contexts_localized_field_not_locked() {
        let mut field = make_field("title", FieldType::Text);
        field.localized = true;
        let fields = vec![field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, true);
        assert_eq!(result[0]["locale_locked"], false);
        assert_eq!(result[0]["readonly"], false);
    }

    // --- build_field_contexts: group field tests ---

    #[test]
    fn build_field_contexts_top_level_group_uses_double_underscore() {
        let mut group = make_field("seo", FieldType::Group);
        group.fields = vec![
            make_field("title", FieldType::Text),
            make_field("description", FieldType::Textarea),
        ];
        let fields = vec![group];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields[0]["name"], "seo__title");
        assert_eq!(sub_fields[1]["name"], "seo__description");
    }

    #[test]
    fn build_field_contexts_group_collapsed() {
        let mut group = make_field("meta", FieldType::Group);
        group.admin.collapsed = true;
        group.fields = vec![make_field("author", FieldType::Text)];
        let fields = vec![group];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["collapsed"], true);
    }

    #[test]
    fn build_field_contexts_group_sub_field_values() {
        let mut group = make_field("seo", FieldType::Group);
        group.fields = vec![make_field("title", FieldType::Text)];
        let mut values = HashMap::new();
        values.insert("seo__title".to_string(), "My SEO Title".to_string());
        let fields = vec![group];
        let result = build_field_contexts(&fields, &values, &HashMap::new(), false, false);
        let sub_fields = result[0]["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields[0]["value"], "My SEO Title");
    }

    // --- build_field_contexts: array with min/max rows and admin options ---

    #[test]
    fn build_field_contexts_array_with_min_max_rows() {
        let mut arr = make_field("items", FieldType::Array);
        arr.fields = vec![make_field("title", FieldType::Text)];
        arr.min_rows = Some(1);
        arr.max_rows = Some(5);
        let fields = vec![arr];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["min_rows"], 1);
        assert_eq!(result[0]["max_rows"], 5);
    }

    #[test]
    fn build_field_contexts_array_init_collapsed() {
        let mut arr = make_field("items", FieldType::Array);
        arr.fields = vec![make_field("title", FieldType::Text)];
        arr.admin.init_collapsed = true;
        let fields = vec![arr];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["init_collapsed"], true);
    }

    #[test]
    fn build_field_contexts_array_labels_singular() {
        let mut arr = make_field("slides", FieldType::Array);
        arr.fields = vec![make_field("title", FieldType::Text)];
        arr.admin.labels_singular = Some(LocalizedString::Plain("Slide".to_string()));
        let fields = vec![arr];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["add_label"], "Slide");
    }

    #[test]
    fn build_field_contexts_array_label_field() {
        let mut arr = make_field("items", FieldType::Array);
        arr.fields = vec![make_field("title", FieldType::Text)];
        arr.admin.label_field = Some("title".to_string());
        let fields = vec![arr];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["label_field"], "title");
    }

    // --- build_field_contexts: blocks with min/max rows and admin options ---

    #[test]
    fn build_field_contexts_blocks_with_min_max_rows() {
        let mut blocks = make_field("content", FieldType::Blocks);
        blocks.blocks = vec![BlockDefinition {
            block_type: "text".to_string(),
            label: None,
            fields: vec![make_field("body", FieldType::Text)],
            ..Default::default()
        }];
        blocks.min_rows = Some(1);
        blocks.max_rows = Some(10);
        let fields = vec![blocks];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["min_rows"], 1);
        assert_eq!(result[0]["max_rows"], 10);
    }

    #[test]
    fn build_field_contexts_blocks_init_collapsed() {
        let mut blocks = make_field("content", FieldType::Blocks);
        blocks.blocks = vec![BlockDefinition {
            block_type: "text".to_string(),
            label: None,
            fields: vec![make_field("body", FieldType::Text)],
            ..Default::default()
        }];
        blocks.admin.init_collapsed = true;
        let fields = vec![blocks];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["init_collapsed"], true);
    }

    #[test]
    fn build_field_contexts_blocks_labels_singular() {
        let mut blocks = make_field("content", FieldType::Blocks);
        blocks.blocks = vec![BlockDefinition {
            block_type: "text".to_string(),
            label: None,
            fields: vec![make_field("body", FieldType::Text)],
            ..Default::default()
        }];
        blocks.admin.labels_singular = Some(LocalizedString::Plain("Block".to_string()));
        let fields = vec![blocks];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["add_label"], "Block");
    }

    #[test]
    fn build_field_contexts_blocks_block_label_field() {
        let mut blocks = make_field("content", FieldType::Blocks);
        blocks.blocks = vec![BlockDefinition {
            block_type: "text".to_string(),
            label: None,
            fields: vec![make_field("body", FieldType::Text)],
            label_field: Some("body".to_string()),
            ..Default::default()
        }];
        let fields = vec![blocks];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        let block_defs = result[0]["block_definitions"].as_array().unwrap();
        assert_eq!(block_defs[0]["label_field"], "body");
    }

    // --- build_field_contexts: position field ---

    #[test]
    fn build_field_contexts_position_set() {
        let mut field = make_field("status", FieldType::Text);
        field.admin.position = Some("sidebar".to_string());
        let fields = vec![field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["position"], "sidebar");
    }

    // --- build_field_contexts: label, placeholder, description ---

    #[test]
    fn build_field_contexts_custom_label_placeholder_description() {
        let mut field = make_field("title", FieldType::Text);
        field.admin.label = Some(LocalizedString::Plain("Custom Title".to_string()));
        field.admin.placeholder = Some(LocalizedString::Plain("Enter title here...".to_string()));
        field.admin.description = Some(LocalizedString::Plain("The main title".to_string()));
        let fields = vec![field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["label"], "Custom Title");
        assert_eq!(result[0]["placeholder"], "Enter title here...");
        assert_eq!(result[0]["description"], "The main title");
    }

    #[test]
    fn build_field_contexts_readonly_field() {
        let mut field = make_field("slug", FieldType::Text);
        field.admin.readonly = true;
        let fields = vec![field];
        let result = build_field_contexts(&fields, &HashMap::new(), &HashMap::new(), false, false);
        assert_eq!(result[0]["readonly"], true);
    }

    // --- build_field_contexts: date short values ---

    #[test]
    fn build_field_contexts_date_short_value_day_only() {
        let mut values = HashMap::new();
        values.insert("d".to_string(), "short".to_string()); // less than 10 chars
        let field = make_field("d", FieldType::Date);
        let fields = vec![field];
        let result = build_field_contexts(&fields, &values, &HashMap::new(), false, false);
        // Should use the short value as-is
        assert_eq!(result[0]["date_only_value"], "short");
    }

    #[test]
    fn build_field_contexts_date_short_value_day_and_time() {
        let mut field = make_field("d", FieldType::Date);
        field.picker_appearance = Some("dayAndTime".to_string());
        let mut values = HashMap::new();
        values.insert("d".to_string(), "short".to_string()); // less than 16 chars
        let fields = vec![field];
        let result = build_field_contexts(&fields, &values, &HashMap::new(), false, false);
        assert_eq!(result[0]["datetime_local_value"], "short");
    }

    // --- apply_field_type_extras tests ---

    #[test]
    fn apply_extras_checkbox_checked() {
        let sf = make_field("active", FieldType::Checkbox);
        let mut ctx = serde_json::json!({"name": "group__active"});
        apply_field_type_extras(&sf, "true", &mut ctx, &HashMap::new(), &HashMap::new(), "group__active", false, 0);
        assert_eq!(ctx["checked"], true);
    }

    #[test]
    fn apply_extras_checkbox_unchecked() {
        let sf = make_field("active", FieldType::Checkbox);
        let mut ctx = serde_json::json!({"name": "group__active"});
        apply_field_type_extras(&sf, "0", &mut ctx, &HashMap::new(), &HashMap::new(), "group__active", false, 0);
        assert_eq!(ctx["checked"], false);
    }

    #[test]
    fn apply_extras_select() {
        let mut sf = make_field("color", FieldType::Select);
        sf.options = vec![
            SelectOption { label: LocalizedString::Plain("Red".to_string()), value: "red".to_string() },
            SelectOption { label: LocalizedString::Plain("Green".to_string()), value: "green".to_string() },
        ];
        let mut ctx = serde_json::json!({"name": "group__color"});
        apply_field_type_extras(&sf, "green", &mut ctx, &HashMap::new(), &HashMap::new(), "group__color", false, 0);
        let opts = ctx["options"].as_array().unwrap();
        assert_eq!(opts[0]["selected"], false);
        assert_eq!(opts[1]["selected"], true);
    }

    #[test]
    fn apply_extras_date_day_only() {
        let sf = make_field("d", FieldType::Date);
        let mut ctx = serde_json::json!({"name": "group__d"});
        apply_field_type_extras(&sf, "2026-01-15T12:00:00Z", &mut ctx, &HashMap::new(), &HashMap::new(), "group__d", false, 0);
        assert_eq!(ctx["picker_appearance"], "dayOnly");
        assert_eq!(ctx["date_only_value"], "2026-01-15");
    }

    #[test]
    fn apply_extras_date_day_and_time() {
        let mut sf = make_field("d", FieldType::Date);
        sf.picker_appearance = Some("dayAndTime".to_string());
        let mut ctx = serde_json::json!({"name": "group__d"});
        apply_field_type_extras(&sf, "2026-01-15T09:30:00Z", &mut ctx, &HashMap::new(), &HashMap::new(), "group__d", false, 0);
        assert_eq!(ctx["picker_appearance"], "dayAndTime");
        assert_eq!(ctx["datetime_local_value"], "2026-01-15T09:30");
    }

    #[test]
    fn apply_extras_date_short_values() {
        let sf = make_field("d", FieldType::Date);
        let mut ctx = serde_json::json!({"name": "g__d"});
        apply_field_type_extras(&sf, "short", &mut ctx, &HashMap::new(), &HashMap::new(), "g__d", false, 0);
        assert_eq!(ctx["date_only_value"], "short");

        let mut sf2 = make_field("d2", FieldType::Date);
        sf2.picker_appearance = Some("dayAndTime".to_string());
        let mut ctx2 = serde_json::json!({"name": "g__d2"});
        apply_field_type_extras(&sf2, "short", &mut ctx2, &HashMap::new(), &HashMap::new(), "g__d2", false, 0);
        assert_eq!(ctx2["datetime_local_value"], "short");
    }

    #[test]
    fn apply_extras_relationship() {
        use crate::core::field::RelationshipConfig;
        let mut sf = make_field("author", FieldType::Relationship);
        sf.relationship = Some(RelationshipConfig {
            collection: "users".to_string(),
            has_many: true,
            max_depth: None,
        });
        let mut ctx = serde_json::json!({"name": "group__author"});
        apply_field_type_extras(&sf, "", &mut ctx, &HashMap::new(), &HashMap::new(), "group__author", false, 0);
        assert_eq!(ctx["relationship_collection"], "users");
        assert_eq!(ctx["has_many"], true);
    }

    #[test]
    fn apply_extras_upload() {
        use crate::core::field::RelationshipConfig;
        let mut sf = make_field("image", FieldType::Upload);
        sf.relationship = Some(RelationshipConfig {
            collection: "media".to_string(),
            has_many: false,
            max_depth: None,
        });
        let mut ctx = serde_json::json!({"name": "group__image"});
        apply_field_type_extras(&sf, "", &mut ctx, &HashMap::new(), &HashMap::new(), "group__image", false, 0);
        assert_eq!(ctx["relationship_collection"], "media");
    }

    #[test]
    fn apply_extras_array_in_group() {
        let mut arr = make_field("tags", FieldType::Array);
        arr.fields = vec![make_field("name", FieldType::Text)];
        arr.min_rows = Some(1);
        arr.max_rows = Some(3);
        arr.admin.init_collapsed = true;
        arr.admin.labels_singular = Some(LocalizedString::Plain("Tag".to_string()));
        arr.admin.label_field = Some("name".to_string());
        let mut ctx = serde_json::json!({"name": "group__tags"});
        apply_field_type_extras(&arr, "", &mut ctx, &HashMap::new(), &HashMap::new(), "group__tags", false, 0);
        assert!(ctx["sub_fields"].as_array().is_some());
        assert_eq!(ctx["row_count"], 0);
        assert_eq!(ctx["min_rows"], 1);
        assert_eq!(ctx["max_rows"], 3);
        assert_eq!(ctx["init_collapsed"], true);
        assert_eq!(ctx["add_label"], "Tag");
        assert_eq!(ctx["label_field"], "name");
    }

    #[test]
    fn apply_extras_group_in_group() {
        let mut inner = make_field("meta", FieldType::Group);
        inner.fields = vec![make_field("author", FieldType::Text)];
        inner.admin.collapsed = true;
        let mut ctx = serde_json::json!({"name": "outer__meta"});
        apply_field_type_extras(&inner, "", &mut ctx, &HashMap::new(), &HashMap::new(), "outer__meta", false, 0);
        assert!(ctx["sub_fields"].as_array().is_some());
        assert_eq!(ctx["collapsed"], true);
    }

    #[test]
    fn apply_extras_blocks_in_group() {
        let mut blk = make_field("sections", FieldType::Blocks);
        blk.blocks = vec![BlockDefinition {
            block_type: "text".to_string(),
            label: None,
            fields: vec![make_field("body", FieldType::Text)],
            label_field: Some("body".to_string()),
            ..Default::default()
        }];
        blk.min_rows = Some(0);
        blk.max_rows = Some(5);
        blk.admin.init_collapsed = true;
        blk.admin.labels_singular = Some(LocalizedString::Plain("Section".to_string()));
        let mut ctx = serde_json::json!({"name": "group__sections"});
        apply_field_type_extras(&blk, "", &mut ctx, &HashMap::new(), &HashMap::new(), "group__sections", false, 0);
        assert!(ctx["block_definitions"].as_array().is_some());
        assert_eq!(ctx["row_count"], 0);
        assert_eq!(ctx["min_rows"], 0);
        assert_eq!(ctx["max_rows"], 5);
        assert_eq!(ctx["init_collapsed"], true);
        assert_eq!(ctx["add_label"], "Section");
        let bd = ctx["block_definitions"].as_array().unwrap();
        assert_eq!(bd[0]["label_field"], "body");
    }

    #[test]
    fn apply_extras_max_depth_stops_recursion() {
        let mut arr = make_field("deep", FieldType::Array);
        arr.fields = vec![make_field("leaf", FieldType::Text)];
        let mut ctx = serde_json::json!({"name": "group__deep"});
        apply_field_type_extras(&arr, "", &mut ctx, &HashMap::new(), &HashMap::new(), "group__deep", false, MAX_FIELD_DEPTH);
        // At max depth, no sub_fields should be added
        assert!(ctx.get("sub_fields").is_none());
    }

    #[test]
    fn apply_extras_unknown_type_is_noop() {
        let sf = make_field("body", FieldType::Richtext);
        let mut ctx = serde_json::json!({"name": "group__body", "field_type": "richtext"});
        apply_field_type_extras(&sf, "hello", &mut ctx, &HashMap::new(), &HashMap::new(), "group__body", false, 0);
        // Should not add any extra fields
        assert!(ctx.get("options").is_none());
        assert!(ctx.get("checked").is_none());
    }

    // --- enriched_sub_field: error propagation ---

    #[test]
    fn enriched_sub_field_with_error() {
        let sf = make_field("title", FieldType::Text);
        let mut errors = HashMap::new();
        errors.insert("content[0][title]".to_string(), "Required".to_string());
        let ctx = build_enriched_sub_field_context(
            &sf, Some(&serde_json::json!("val")), "content", 0,
            false, false, 1, &errors,
        );
        assert_eq!(ctx["error"], "Required");
    }

    // --- enriched_sub_field: max depth ---

    #[test]
    fn enriched_sub_field_max_depth_returns_early() {
        let mut arr = make_field("deep", FieldType::Array);
        arr.fields = vec![make_field("leaf", FieldType::Text)];
        let ctx = build_enriched_sub_field_context(
            &arr, Some(&serde_json::json!([])), "parent", 0,
            false, false, MAX_FIELD_DEPTH, &HashMap::new(),
        );
        // At max depth, array-specific fields should not be added
        assert!(ctx.get("rows").is_none());
        assert!(ctx.get("sub_fields").is_none());
    }

    // --- enriched_sub_field: date field ---

    #[test]
    fn enriched_sub_field_date_day_only() {
        let sf = make_field("d", FieldType::Date);
        let raw = serde_json::json!("2026-03-15T10:00:00Z");
        let ctx = build_enriched_sub_field_context(
            &sf, Some(&raw), "items", 0, false, false, 1, &HashMap::new(),
        );
        assert_eq!(ctx["picker_appearance"], "dayOnly");
        assert_eq!(ctx["date_only_value"], "2026-03-15");
    }

    #[test]
    fn enriched_sub_field_date_day_and_time() {
        let mut sf = make_field("d", FieldType::Date);
        sf.picker_appearance = Some("dayAndTime".to_string());
        let raw = serde_json::json!("2026-03-15T10:30:00Z");
        let ctx = build_enriched_sub_field_context(
            &sf, Some(&raw), "items", 0, false, false, 1, &HashMap::new(),
        );
        assert_eq!(ctx["picker_appearance"], "dayAndTime");
        assert_eq!(ctx["datetime_local_value"], "2026-03-15T10:30");
    }

    #[test]
    fn enriched_sub_field_date_short_value() {
        let sf = make_field("d", FieldType::Date);
        let raw = serde_json::json!("short");
        let ctx = build_enriched_sub_field_context(
            &sf, Some(&raw), "items", 0, false, false, 1, &HashMap::new(),
        );
        assert_eq!(ctx["date_only_value"], "short");
    }

    // --- enriched_sub_field: upload field ---

    #[test]
    fn enriched_sub_field_upload() {
        use crate::core::field::RelationshipConfig;
        let mut sf = make_field("image", FieldType::Upload);
        sf.relationship = Some(RelationshipConfig {
            collection: "media".to_string(),
            has_many: false,
            max_depth: None,
        });
        let ctx = build_enriched_sub_field_context(
            &sf, Some(&serde_json::json!("img123")), "items", 0,
            false, false, 1, &HashMap::new(),
        );
        assert_eq!(ctx["relationship_collection"], "media");
    }

    // --- enriched_sub_field: relationship field ---

    #[test]
    fn enriched_sub_field_relationship() {
        use crate::core::field::RelationshipConfig;
        let mut sf = make_field("author", FieldType::Relationship);
        sf.relationship = Some(RelationshipConfig {
            collection: "users".to_string(),
            has_many: true,
            max_depth: None,
        });
        let ctx = build_enriched_sub_field_context(
            &sf, Some(&serde_json::json!("user1")), "items", 0,
            false, false, 1, &HashMap::new(),
        );
        assert_eq!(ctx["relationship_collection"], "users");
        assert_eq!(ctx["has_many"], true);
    }

    // --- enriched_sub_field: value stringification ---

    #[test]
    fn enriched_sub_field_null_value_empty_string() {
        let sf = make_field("title", FieldType::Text);
        let ctx = build_enriched_sub_field_context(
            &sf, Some(&serde_json::Value::Null), "items", 0,
            false, false, 1, &HashMap::new(),
        );
        assert_eq!(ctx["value"], "");
    }

    #[test]
    fn enriched_sub_field_number_to_string() {
        let sf = make_field("count", FieldType::Number);
        let ctx = build_enriched_sub_field_context(
            &sf, Some(&serde_json::json!(42)), "items", 0,
            false, false, 1, &HashMap::new(),
        );
        assert_eq!(ctx["value"], "42");
    }

    #[test]
    fn enriched_sub_field_no_value() {
        let sf = make_field("title", FieldType::Text);
        let ctx = build_enriched_sub_field_context(
            &sf, None, "items", 0, false, false, 1, &HashMap::new(),
        );
        assert_eq!(ctx["value"], "");
    }

    // --- enriched_sub_field: array with min/max rows, init_collapsed, labels ---

    #[test]
    fn enriched_sub_field_array_with_options() {
        let mut arr = make_field("tags", FieldType::Array);
        arr.fields = vec![make_field("name", FieldType::Text)];
        arr.min_rows = Some(1);
        arr.max_rows = Some(5);
        arr.admin.init_collapsed = true;
        arr.admin.labels_singular = Some(LocalizedString::Plain("Tag".to_string()));
        let ctx = build_enriched_sub_field_context(
            &arr, Some(&serde_json::json!([])), "items", 0,
            false, false, 1, &HashMap::new(),
        );
        assert_eq!(ctx["min_rows"], 1);
        assert_eq!(ctx["max_rows"], 5);
        assert_eq!(ctx["init_collapsed"], true);
        assert_eq!(ctx["add_label"], "Tag");
    }

    // --- enriched_sub_field: blocks with min/max rows, init_collapsed, labels ---

    #[test]
    fn enriched_sub_field_blocks_with_options() {
        let mut blk = make_field("sections", FieldType::Blocks);
        blk.blocks = vec![BlockDefinition {
            block_type: "text".to_string(),
            label: None,
            fields: vec![make_field("body", FieldType::Text)],
            ..Default::default()
        }];
        blk.min_rows = Some(0);
        blk.max_rows = Some(10);
        blk.admin.init_collapsed = true;
        blk.admin.labels_singular = Some(LocalizedString::Plain("Section".to_string()));
        blk.admin.label_field = Some("body".to_string());
        let ctx = build_enriched_sub_field_context(
            &blk, Some(&serde_json::json!([])), "items", 0,
            false, false, 1, &HashMap::new(),
        );
        assert_eq!(ctx["min_rows"], 0);
        assert_eq!(ctx["max_rows"], 10);
        assert_eq!(ctx["init_collapsed"], true);
        assert_eq!(ctx["add_label"], "Section");
        assert_eq!(ctx["label_field"], "body");
    }

    // --- enriched_sub_field: nested blocks with row errors ---

    #[test]
    fn enriched_sub_field_nested_array_row_errors() {
        let mut inner_array = make_field("items", FieldType::Array);
        inner_array.fields = vec![make_field("title", FieldType::Text)];

        let raw_value = serde_json::json!([{"title": ""}]);
        let mut errors = HashMap::new();
        errors.insert("parent[0][items][0][title]".to_string(), "Required".to_string());

        let ctx = build_enriched_sub_field_context(
            &inner_array, Some(&raw_value), "parent", 0,
            false, false, 1, &errors,
        );

        let rows = ctx["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        let row_fields = rows[0]["sub_fields"].as_array().unwrap();
        assert_eq!(row_fields[0]["error"], "Required");
        assert_eq!(rows[0]["has_errors"], true);
    }

    #[test]
    fn enriched_sub_field_nested_blocks_row_errors() {
        let mut blk = make_field("sections", FieldType::Blocks);
        blk.blocks = vec![BlockDefinition {
            block_type: "text".to_string(),
            label: Some(LocalizedString::Plain("Text".to_string())),
            fields: vec![make_field("body", FieldType::Richtext)],
            ..Default::default()
        }];

        let raw_value = serde_json::json!([{"_block_type": "text", "body": ""}]);
        let mut errors = HashMap::new();
        errors.insert("parent[0][sections][0][body]".to_string(), "Required".to_string());

        let ctx = build_enriched_sub_field_context(
            &blk, Some(&raw_value), "parent", 0,
            false, false, 1, &errors,
        );

        let rows = ctx["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["has_errors"], true);
    }

    // --- enriched_sub_field: group with collapsed ---

    #[test]
    fn enriched_sub_field_group_collapsed() {
        let mut grp = make_field("meta", FieldType::Group);
        grp.fields = vec![make_field("author", FieldType::Text)];
        grp.admin.collapsed = true;
        let raw = serde_json::json!({"author": "Alice"});
        let ctx = build_enriched_sub_field_context(
            &grp, Some(&raw), "items", 0,
            false, false, 1, &HashMap::new(),
        );
        assert_eq!(ctx["collapsed"], true);
    }

    // --- enriched_sub_field: group with non-object value ---

    #[test]
    fn enriched_sub_field_group_with_null_value() {
        let mut grp = make_field("meta", FieldType::Group);
        grp.fields = vec![make_field("author", FieldType::Text)];
        let ctx = build_enriched_sub_field_context(
            &grp, Some(&serde_json::Value::Null), "items", 0,
            false, false, 1, &HashMap::new(),
        );
        // group_obj should be None so nested values are empty
        let sub_fields = ctx["sub_fields"].as_array().unwrap();
        assert_eq!(sub_fields[0]["value"], "");
    }

    // --- enriched_sub_field: nested blocks with unknown block type ---

    #[test]
    fn enriched_sub_field_nested_blocks_unknown_type() {
        let mut blk = make_field("sections", FieldType::Blocks);
        blk.blocks = vec![BlockDefinition {
            block_type: "text".to_string(),
            label: Some(LocalizedString::Plain("Text".to_string())),
            fields: vec![make_field("body", FieldType::Richtext)],
            ..Default::default()
        }];

        // Row with unknown block type
        let raw_value = serde_json::json!([{"_block_type": "unknown_type", "body": "content"}]);

        let ctx = build_enriched_sub_field_context(
            &blk, Some(&raw_value), "parent", 0,
            false, false, 1, &HashMap::new(),
        );

        let rows = ctx["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["_block_type"], "unknown_type");
        assert_eq!(rows[0]["block_label"], "unknown_type"); // falls back to block_type string
        // sub_fields should be empty since block_def is not found
        let sub_fields = rows[0]["sub_fields"].as_array().unwrap();
        assert!(sub_fields.is_empty());
    }

    // --- enrich_nested_fields tests ---

    #[test]
    fn enrich_nested_fields_upload_gets_options() {
        use crate::core::collection::*;
        use crate::core::field::RelationshipConfig;

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE media (
                id TEXT PRIMARY KEY,
                alt TEXT,
                caption TEXT,
                filename TEXT,
                mime_type TEXT,
                url TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO media (id, alt, filename, mime_type, url, created_at, updated_at)
            VALUES ('img1', 'Logo', 'logo.png', 'image/png', '/uploads/media/logo.png', '2024-01-01', '2024-01-01');
            INSERT INTO media (id, alt, filename, mime_type, url, created_at, updated_at)
            VALUES ('img2', 'Banner', 'banner.jpg', 'image/jpeg', '/uploads/media/banner.jpg', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let media_def = CollectionDefinition {
            slug: "media".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                make_field("alt", FieldType::Text),
                make_field("caption", FieldType::Text),
                make_field("filename", FieldType::Text),
                make_field("mime_type", FieldType::Text),
                make_field("url", FieldType::Text),
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: Some(crate::core::upload::CollectionUpload {
                enabled: true,
                mime_types: vec!["image/*".to_string()],
                max_file_size: None,
                image_sizes: vec![],
                admin_thumbnail: None,
                format_options: Default::default(),
            }),
            access: CollectionAccess::default(),
            live: None,
            versions: None,
        };

        let mut registry = crate::core::Registry::new();
        registry.register_collection(media_def);

        let mut upload_field = make_field("image", FieldType::Upload);
        upload_field.relationship = Some(RelationshipConfig {
            collection: "media".to_string(),
            has_many: false,
            max_depth: None,
        });

        let field_defs = vec![upload_field];
        let mut sub_fields = vec![serde_json::json!({
            "name": "content[0][image]",
            "field_type": "upload",
            "value": "",
            "relationship_collection": "media",
        })];

        enrich_nested_fields(&mut sub_fields, &field_defs, &conn, &registry, None);

        let options = sub_fields[0]["relationship_options"].as_array()
            .expect("relationship_options should be populated");
        assert_eq!(options.len(), 2, "Should have 2 media options");
        assert!(options.iter().any(|o| o["value"] == "img1"), "Should contain img1");
        assert!(options.iter().any(|o| o["value"] == "img2"), "Should contain img2");
    }

    #[test]
    fn enrich_nested_fields_relationship_gets_options() {
        use crate::core::collection::*;
        use crate::core::field::RelationshipConfig;

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (
                id TEXT PRIMARY KEY,
                name TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO users (id, name, created_at, updated_at)
            VALUES ('u1', 'Alice', '2024-01-01', '2024-01-01');
            INSERT INTO users (id, name, created_at, updated_at)
            VALUES ('u2', 'Bob', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let users_def = CollectionDefinition {
            slug: "users".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![make_field("name", FieldType::Text)],
            admin: CollectionAdmin {
                use_as_title: Some("name".to_string()),
                ..Default::default()
            },
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            live: None,
            versions: None,
        };

        let mut registry = crate::core::Registry::new();
        registry.register_collection(users_def);

        let mut rel_field = make_field("author", FieldType::Relationship);
        rel_field.relationship = Some(RelationshipConfig {
            collection: "users".to_string(),
            has_many: false,
            max_depth: None,
        });

        let field_defs = vec![rel_field];
        let mut sub_fields = vec![serde_json::json!({
            "name": "items[0][author]",
            "field_type": "relationship",
            "value": "u1",
            "relationship_collection": "users",
        })];

        enrich_nested_fields(&mut sub_fields, &field_defs, &conn, &registry, None);

        let options = sub_fields[0]["relationship_options"].as_array()
            .expect("relationship_options should be populated");
        assert_eq!(options.len(), 2, "Should have 2 user options");
        let alice = options.iter().find(|o| o["value"] == "u1").unwrap();
        assert_eq!(alice["label"], "Alice");
        assert_eq!(alice["selected"], true, "u1 should be selected");
    }

    #[test]
    fn enrich_nested_fields_recurses_into_layout() {
        use crate::core::collection::*;
        use crate::core::field::RelationshipConfig;

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE tags (
                id TEXT PRIMARY KEY,
                label TEXT,
                created_at TEXT,
                updated_at TEXT
            );
            INSERT INTO tags (id, label, created_at, updated_at)
            VALUES ('t1', 'Rust', '2024-01-01', '2024-01-01');"
        ).unwrap();

        let tags_def = CollectionDefinition {
            slug: "tags".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![make_field("label", FieldType::Text)],
            admin: CollectionAdmin {
                use_as_title: Some("label".to_string()),
                ..Default::default()
            },
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            live: None,
            versions: None,
        };

        let mut registry = crate::core::Registry::new();
        registry.register_collection(tags_def);

        // A Row containing a Relationship field
        let mut rel_field = make_field("tag", FieldType::Relationship);
        rel_field.relationship = Some(RelationshipConfig {
            collection: "tags".to_string(),
            has_many: false,
            max_depth: None,
        });
        let row_field = FieldDefinition {
            name: "row1".to_string(),
            field_type: FieldType::Row,
            fields: vec![rel_field],
            ..Default::default()
        };

        let field_defs = vec![row_field];
        let mut sub_fields = vec![serde_json::json!({
            "name": "row1",
            "field_type": "row",
            "sub_fields": [{
                "name": "tag",
                "field_type": "relationship",
                "value": "",
                "relationship_collection": "tags",
            }],
        })];

        enrich_nested_fields(&mut sub_fields, &field_defs, &conn, &registry, None);

        let row_subs = sub_fields[0]["sub_fields"].as_array().unwrap();
        let options = row_subs[0]["relationship_options"].as_array()
            .expect("Nested relationship inside Row should be enriched");
        assert_eq!(options.len(), 1);
        assert_eq!(options[0]["label"], "Rust");
    }
}
