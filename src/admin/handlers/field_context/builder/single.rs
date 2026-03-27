//! Build a single field context for template rendering.

use std::collections::HashMap;

use serde_json::{Value, from_str, json};

use crate::{
    admin::handlers::shared::auto_label_from_name,
    core::field::{FieldDefinition, FieldType},
};

use super::super::{
    MAX_FIELD_DEPTH, collect_node_attr_errors, count_errors_in_fields, safe_template_id,
};
use super::build_select_options;

/// Resolve the full form name for a field, accounting for layout transparency.
fn resolve_full_name(field: &FieldDefinition, name_prefix: &str) -> String {
    if name_prefix.is_empty() {
        field.name.clone()
    } else if matches!(
        field.field_type,
        FieldType::Tabs | FieldType::Row | FieldType::Collapsible
    ) {
        name_prefix.to_string() // transparent — layout wrappers don't add their name
    } else if !name_prefix.contains('[') {
        // Top-level group chain: continue using __ naming (matches DB columns)
        format!("{}__{}", name_prefix, field.name)
    } else {
        format!("{}[{}]", name_prefix, field.name)
    }
}

/// Build base field context with common properties and validation attributes.
/// Returns (ctx, full_name, value).
fn build_base_field_context(
    field: &FieldDefinition,
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    name_prefix: &str,
    non_default_locale: bool,
) -> (Value, String, String) {
    let full_name = resolve_full_name(field, name_prefix);
    let value = values.get(&full_name).cloned().unwrap_or_default();

    let label = field
        .admin
        .label
        .as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| auto_label_from_name(&field.name));

    let locale_locked = non_default_locale && !field.localized;

    let mut ctx = json!({
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
        ctx["position"] = json!(pos);
    }
    if let Some(err) = errors.get(&full_name) {
        ctx["error"] = json!(err);
    }

    // Validation properties
    if let Some(ml) = field.min_length {
        ctx["min_length"] = json!(ml);
    }
    if let Some(ml) = field.max_length {
        ctx["max_length"] = json!(ml);
    }
    if let Some(v) = field.min {
        ctx["min"] = json!(v);
        ctx["has_min"] = json!(true);
    }
    if let Some(v) = field.max {
        ctx["max"] = json!(v);
        ctx["has_max"] = json!(true);
    }
    if field.field_type == FieldType::Number {
        ctx["step"] = json!(field.admin.step.as_deref().unwrap_or("any"));
    }
    if field.field_type == FieldType::Textarea {
        ctx["rows"] = json!(field.admin.rows.unwrap_or(8));
        ctx["resizable"] = json!(field.admin.resizable);
    }
    if field.field_type == FieldType::Date {
        if let Some(ref md) = field.min_date {
            ctx["min_date"] = json!(md);
        }
        if let Some(ref md) = field.max_date {
            ctx["max_date"] = json!(md);
        }
    }
    if field.field_type == FieldType::Code {
        ctx["language"] = json!(field.admin.language.as_deref().unwrap_or("json"));
    }

    (ctx, full_name, value)
}

/// Build a field context for a single field definition, recursing into composite sub-fields.
///
/// `name_prefix`: the full form-name prefix for this field (e.g. `"content[0]"` for a
/// field inside a blocks row at index 0). Top-level fields use an empty prefix.
/// `depth`: current nesting depth (0 = top-level). Stops recursing at MAX_FIELD_DEPTH.
pub fn build_single_field_context(
    field: &FieldDefinition,
    values: &HashMap<String, String>,
    errors: &HashMap<String, String>,
    name_prefix: &str,
    non_default_locale: bool,
    depth: usize,
) -> Value {
    let (mut ctx, full_name, value) =
        build_base_field_context(field, values, errors, name_prefix, non_default_locale);

    // Beyond max depth, render as a simple text input
    if depth >= MAX_FIELD_DEPTH {
        return ctx;
    }

    match &field.field_type {
        FieldType::Select | FieldType::Radio => {
            let (options, has_many) = build_select_options(field, &value);
            ctx["options"] = json!(options);
            if has_many {
                ctx["has_many"] = json!(true);
            }
        }
        FieldType::Checkbox => {
            let checked = matches!(value.as_str(), "1" | "true" | "on" | "yes");
            ctx["checked"] = json!(checked);
        }
        FieldType::Relationship => {
            if let Some(ref rc) = field.relationship {
                ctx["relationship_collection"] = json!(rc.collection);
                ctx["has_many"] = json!(rc.has_many);
                if rc.is_polymorphic() {
                    ctx["polymorphic"] = json!(true);
                    ctx["collections"] = json!(rc.polymorphic);
                }
            }

            if let Some(ref p) = field.admin.picker {
                ctx["picker"] = json!(p);
            }
        }
        FieldType::Array => {
            // Build sub_field contexts for the <template> section (with __INDEX__ placeholder)
            let template_prefix = format!("{}[__INDEX__]", full_name);
            let sub_fields: Vec<_> = field
                .fields
                .iter()
                .map(|sf| {
                    build_single_field_context(
                        sf,
                        &HashMap::new(),
                        &HashMap::new(),
                        &template_prefix,
                        non_default_locale,
                        depth + 1,
                    )
                })
                .collect();

            ctx["sub_fields"] = json!(sub_fields);
            ctx["row_count"] = json!(0);
            ctx["template_id"] = json!(safe_template_id(&full_name));

            if let Some(ref lf) = field.admin.label_field {
                ctx["label_field"] = json!(lf);
            }

            if let Some(max) = field.max_rows {
                ctx["max_rows"] = json!(max);
            }

            if let Some(min) = field.min_rows {
                ctx["min_rows"] = json!(min);
            }

            ctx["init_collapsed"] = json!(field.admin.collapsed);

            if let Some(ref ls) = field.admin.labels_singular {
                ctx["add_label"] = json!(ls.resolve_default());
            }
        }
        FieldType::Group => {
            // Use recursive path for both top-level and nested groups.
            // resolve_full_name handles __ vs bracket naming and layout transparency.
            let prefix = if name_prefix.is_empty() {
                field.name.clone()
            } else {
                full_name.clone()
            };

            // If the group is localized, all children are editable in any locale.
            let child_non_default_locale = if field.localized {
                false
            } else {
                non_default_locale
            };

            let sub_fields: Vec<_> = field
                .fields
                .iter()
                .map(|sf| {
                    build_single_field_context(
                        sf,
                        values,
                        errors,
                        &prefix,
                        child_non_default_locale,
                        depth + 1,
                    )
                })
                .collect();

            ctx["sub_fields"] = json!(sub_fields);
            ctx["collapsed"] = json!(field.admin.collapsed);
        }
        FieldType::Row => {
            // Row is a layout-only container; sub-fields are promoted to top level.
            // Top-level row promotes sub-fields to the same level as the parent,
            // so we delegate to build_single_field_context with the same prefix.
            // This correctly handles Group (double-underscore), Collapsible, etc.
            let sub_fields: Vec<_> = if name_prefix.is_empty() {
                field
                    .fields
                    .iter()
                    .map(|sf| {
                        build_single_field_context(
                            sf,
                            values,
                            errors,
                            "",
                            non_default_locale,
                            depth + 1,
                        )
                    })
                    .collect()
            } else {
                // Nested row: use bracketed naming via recursion
                field
                    .fields
                    .iter()
                    .map(|sf| {
                        build_single_field_context(
                            sf,
                            values,
                            errors,
                            &full_name,
                            non_default_locale,
                            depth + 1,
                        )
                    })
                    .collect()
            };

            ctx["sub_fields"] = json!(sub_fields);
        }
        FieldType::Collapsible => {
            // Collapsible is a layout-only container like Row but with a toggle header.
            // Top-level collapsible promotes sub-fields to the same level as the parent,
            // so we delegate to build_single_field_context with the same prefix.
            // This correctly handles Group (double-underscore), Row, etc.
            let sub_fields: Vec<_> = if name_prefix.is_empty() {
                field
                    .fields
                    .iter()
                    .map(|sf| {
                        build_single_field_context(
                            sf,
                            values,
                            errors,
                            "",
                            non_default_locale,
                            depth + 1,
                        )
                    })
                    .collect()
            } else {
                field
                    .fields
                    .iter()
                    .map(|sf| {
                        build_single_field_context(
                            sf,
                            values,
                            errors,
                            &full_name,
                            non_default_locale,
                            depth + 1,
                        )
                    })
                    .collect()
            };

            ctx["sub_fields"] = json!(sub_fields);
            ctx["collapsed"] = json!(field.admin.collapsed);
        }
        FieldType::Tabs => {
            // Tabs is a layout-only container with multiple tab panels.
            // Top-level tabs promote sub-fields to the same level as the parent,
            // so we delegate to build_single_field_context with the same prefix.
            // This correctly handles Group (double-underscore), Row, Collapsible, etc.
            let tabs_ctx: Vec<_> = field
                .tabs
                .iter()
                .map(|tab| {
                    let tab_sub_fields: Vec<_> = if name_prefix.is_empty() {
                        tab.fields
                            .iter()
                            .map(|sf| {
                                build_single_field_context(
                                    sf,
                                    values,
                                    errors,
                                    "",
                                    non_default_locale,
                                    depth + 1,
                                )
                            })
                            .collect()
                    } else {
                        tab.fields
                            .iter()
                            .map(|sf| {
                                build_single_field_context(
                                    sf,
                                    values,
                                    errors,
                                    &full_name,
                                    non_default_locale,
                                    depth + 1,
                                )
                            })
                            .collect()
                    };

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

            ctx["tabs"] = json!(tabs_ctx);
        }
        FieldType::Date => {
            let appearance = field.picker_appearance.as_deref().unwrap_or("dayOnly");

            ctx["picker_appearance"] = json!(appearance);

            let tz_key = format!("{}_tz", full_name);
            let tz_value = values.get(&tz_key).map(|s| s.as_str()).unwrap_or("");

            // If a timezone is stored, convert UTC back to local time for display
            let tz_value = tz_value.trim();
            let display_value = if !tz_value.is_empty() && !value.is_empty() {
                crate::db::query::helpers::utc_to_local(&value, tz_value)
                    .unwrap_or_else(|| value.clone())
            } else {
                value.clone()
            };

            match appearance {
                "dayOnly" => {
                    let date_val = display_value.get(..10).unwrap_or(&display_value);
                    ctx["date_only_value"] = json!(date_val);
                }
                "dayAndTime" => {
                    let dt_val = display_value.get(..16).unwrap_or(&display_value);
                    ctx["datetime_local_value"] = json!(dt_val);
                }
                _ => {}
            }

            super::super::add_timezone_context(&mut ctx, field, tz_value, "");
        }
        FieldType::Upload => {
            if let Some(ref rc) = field.relationship {
                ctx["relationship_collection"] = json!(rc.collection);

                if rc.has_many {
                    ctx["has_many"] = json!(true);
                }
            }

            let picker = field.admin.picker.as_deref().unwrap_or("drawer");
            if picker != "none" {
                ctx["picker"] = json!(picker);
            }
        }
        FieldType::Text | FieldType::Number if field.has_many => {
            // Tag-style input: value is a JSON array like ["tag1","tag2"]
            let tags: Vec<String> = from_str(&value).unwrap_or_default();

            ctx["has_many"] = json!(true);
            ctx["tags"] = json!(tags);
            // Store comma-separated for the hidden input
            ctx["value"] = json!(tags.join(","));
        }
        FieldType::Richtext => {
            ctx["resizable"] = json!(field.admin.resizable);

            if !field.admin.features.is_empty() {
                ctx["features"] = json!(field.admin.features);
            }

            let fmt = field.admin.richtext_format.as_deref().unwrap_or("html");

            ctx["richtext_format"] = json!(fmt);

            // Store node names — full defs resolved in enrich_field_contexts
            if !field.admin.nodes.is_empty() {
                ctx["_node_names"] = json!(field.admin.nodes);
            }

            // Attach node attr validation errors (e.g. content[cta#0].text)
            if ctx.get("error").is_none_or(|v| v.is_null())
                && let Some(node_err) = collect_node_attr_errors(errors, &full_name)
            {
                ctx["error"] = json!(node_err);
            }
        }
        FieldType::Blocks => {
            let block_defs: Vec<_> = field.blocks.iter().map(|bd| {
                // Build sub-field contexts for each block type's <template> section
                let template_prefix = format!("{}[__INDEX__]", full_name);
                let block_fields: Vec<_> = bd.fields.iter().map(|sf| {
                    build_single_field_context(sf, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
                }).collect();
                let mut def = json!({
                    "block_type": bd.block_type,
                    "label": bd.label.as_ref().map(|ls| ls.resolve_default()).unwrap_or(&bd.block_type),
                    "fields": block_fields,
                });

                if let Some(ref lf) = bd.label_field {
                    def["label_field"] = json!(lf);
                }

                if let Some(ref g) = bd.group {
                    def["group"] = json!(g);
                }

                if let Some(ref url) = bd.image_url {
                    def["image_url"] = json!(url);
                }

                def
            }).collect();

            ctx["block_definitions"] = json!(block_defs);
            ctx["row_count"] = json!(0);
            ctx["template_id"] = json!(safe_template_id(&full_name));

            if let Some(ref lf) = field.admin.label_field {
                ctx["label_field"] = json!(lf);
            }

            if let Some(max) = field.max_rows {
                ctx["max_rows"] = json!(max);
            }

            if let Some(min) = field.min_rows {
                ctx["min_rows"] = json!(min);
            }

            ctx["init_collapsed"] = json!(field.admin.collapsed);

            if let Some(ref ls) = field.admin.labels_singular {
                ctx["add_label"] = json!(ls.resolve_default());
            }

            if let Some(ref p) = field.admin.picker {
                ctx["picker"] = json!(p);
            }
        }
        FieldType::Join => {
            if let Some(ref jc) = field.join {
                ctx["join_collection"] = json!(jc.collection);
                ctx["join_on"] = json!(jc.on);
            }

            ctx["readonly"] = json!(true);
        }
        _ => {}
    }

    ctx
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::core::field::{FieldDefinition, FieldType};

    use super::build_single_field_context;

    fn group_field(name: &str, localized: bool, children: Vec<FieldDefinition>) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Group,
            localized,
            fields: children,
            ..Default::default()
        }
    }

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::Text,
            ..Default::default()
        }
    }

    #[test]
    fn non_localized_group_in_non_default_locale_locks_children() {
        let field = group_field("meta", false, vec![text_field("title")]);
        let values = HashMap::new();
        let errors = HashMap::new();

        let ctx = build_single_field_context(&field, &values, &errors, "", true, 0);

        // The group itself should be locale-locked
        assert_eq!(ctx["locale_locked"], true);

        // Children should inherit locale lock (non_default_locale=true, group not localized)
        let sub = &ctx["sub_fields"][0];
        assert_eq!(
            sub["locale_locked"], true,
            "child of non-localized group must be locale_locked in non-default locale"
        );
        assert_eq!(sub["readonly"], true);
    }

    #[test]
    fn localized_group_in_non_default_locale_unlocks_children() {
        let field = group_field("meta", true, vec![text_field("title")]);
        let values = HashMap::new();
        let errors = HashMap::new();

        let ctx = build_single_field_context(&field, &values, &errors, "", true, 0);

        // The localized group itself should NOT be locale-locked
        assert_eq!(ctx["locale_locked"], false);

        // Children should be editable (non_default_locale reset to false for localized group)
        let sub = &ctx["sub_fields"][0];
        assert_eq!(
            sub["locale_locked"], false,
            "child of localized group must NOT be locale_locked"
        );
        assert_eq!(sub["readonly"], false);
    }
}
