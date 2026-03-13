//! Build field context objects for template rendering (no DB access).

use super::{MAX_FIELD_DEPTH, count_errors_in_fields, safe_template_id};
use serde_json::{Value, from_str, json};

use std::collections::{HashMap, HashSet};

use crate::{
    admin::handlers::shared::auto_label_from_name,
    core::field::{FieldDefinition, FieldType},
};

/// Parameters for recursive child-field building inside composite types
/// (Group, Array, Blocks, Tabs, etc.).
pub struct FieldRecursionCtx<'a> {
    pub values: &'a HashMap<String, String>,
    pub errors: &'a HashMap<String, String>,
    pub name_prefix: &'a str,
    pub non_default_locale: bool,
    pub depth: usize,
}

impl<'a> FieldRecursionCtx<'a> {
    pub fn builder(
        values: &'a HashMap<String, String>,
        errors: &'a HashMap<String, String>,
        name_prefix: &'a str,
    ) -> FieldRecursionCtxBuilder<'a> {
        FieldRecursionCtxBuilder {
            values,
            errors,
            name_prefix,
            non_default_locale: false,
            depth: 0,
        }
    }
}

/// Builder for [`FieldRecursionCtx`].
pub struct FieldRecursionCtxBuilder<'a> {
    values: &'a HashMap<String, String>,
    errors: &'a HashMap<String, String>,
    name_prefix: &'a str,
    non_default_locale: bool,
    depth: usize,
}

impl<'a> FieldRecursionCtxBuilder<'a> {
    pub fn non_default_locale(mut self, v: bool) -> Self {
        self.non_default_locale = v;
        self
    }

    pub fn depth(mut self, v: usize) -> Self {
        self.depth = v;
        self
    }

    pub fn build(self) -> FieldRecursionCtx<'a> {
        FieldRecursionCtx {
            values: self.values,
            errors: self.errors,
            name_prefix: self.name_prefix,
            non_default_locale: self.non_default_locale,
            depth: self.depth,
        }
    }
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
    let full_name = if name_prefix.is_empty() {
        field.name.clone()
    } else if matches!(
        field.field_type,
        FieldType::Tabs | FieldType::Row | FieldType::Collapsible
    ) {
        name_prefix.to_string() // transparent — layout wrappers don't add their name
    } else {
        format!("{}[{}]", name_prefix, field.name)
    };

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

    // Validation property context: min_length, max_length, min, max, step, rows
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
    // Number step: use admin.step or default "any"
    if field.field_type == FieldType::Number {
        let step = field.admin.step.as_deref().unwrap_or("any");
        ctx["step"] = json!(step);
    }
    // Textarea rows: use admin.rows or default 8
    if field.field_type == FieldType::Textarea {
        let rows = field.admin.rows.unwrap_or(8);
        ctx["rows"] = json!(rows);
    }
    // Date bounds: min_date / max_date
    if field.field_type == FieldType::Date {
        if let Some(ref md) = field.min_date {
            ctx["min_date"] = json!(md);
        }
        if let Some(ref md) = field.max_date {
            ctx["max_date"] = json!(md);
        }
    }
    // Code language: use admin.language or default "json"
    if field.field_type == FieldType::Code {
        let lang = field.admin.language.as_deref().unwrap_or("json");
        ctx["language"] = json!(lang);
    }

    // Beyond max depth, render as a simple text input
    if depth >= MAX_FIELD_DEPTH {
        return ctx;
    }

    match &field.field_type {
        FieldType::Select | FieldType::Radio => {
            if field.has_many {
                // Multi-select: value is a JSON array like ["val1","val2"]
                let selected_values: HashSet<String> = from_str(&value).unwrap_or_default();

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

                ctx["options"] = json!(options);
                ctx["has_many"] = json!(true);
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

                ctx["options"] = json!(options);
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

                    let mut sub_ctx = json!({
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
                    let extras_ctx = FieldRecursionCtx::builder(values, errors, &col_name)
                        .non_default_locale(non_default_locale)
                        .depth(depth + 1)
                        .build();
                    apply_field_type_extras(sf, &sub_value, &mut sub_ctx, &extras_ctx);

                    sub_ctx
                }).collect()
            } else {
                // Nested group: use bracketed naming via recursion
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

            match appearance {
                "dayOnly" => {
                    let date_val = if value.len() >= 10 {
                        &value[..10]
                    } else {
                        &value
                    };

                    ctx["date_only_value"] = json!(date_val);
                }
                "dayAndTime" => {
                    let dt_val = if value.len() >= 16 {
                        &value[..16]
                    } else {
                        &value
                    };

                    ctx["datetime_local_value"] = json!(dt_val);
                }
                _ => {}
            }
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
            if !field.admin.features.is_empty() {
                ctx["features"] = json!(field.admin.features);
            }

            let fmt = field.admin.richtext_format.as_deref().unwrap_or("html");

            ctx["richtext_format"] = json!(fmt);

            // Store node names — full defs resolved in enrich_field_contexts
            if !field.admin.nodes.is_empty() {
                ctx["_node_names"] = json!(field.admin.nodes);
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

/// Apply type-specific extras to an already-built sub_ctx (for top-level group sub-fields
/// that use the `col_name` pattern but still need composite-type recursion).
pub fn apply_field_type_extras(
    sf: &FieldDefinition,
    value: &str,
    sub_ctx: &mut Value,
    extras: &FieldRecursionCtx,
) {
    let values = extras.values;
    let errors = extras.errors;
    let name_prefix = extras.name_prefix;
    let non_default_locale = extras.non_default_locale;
    let depth = extras.depth;
    // Validation property context for sub-fields
    if let Some(ml) = sf.min_length {
        sub_ctx["min_length"] = json!(ml);
    }

    if let Some(ml) = sf.max_length {
        sub_ctx["max_length"] = json!(ml);
    }

    if let Some(v) = sf.min {
        sub_ctx["min"] = json!(v);
        sub_ctx["has_min"] = json!(true);
    }

    if let Some(v) = sf.max {
        sub_ctx["max"] = json!(v);
        sub_ctx["has_max"] = json!(true);
    }

    if sf.field_type == FieldType::Number {
        let step = sf.admin.step.as_deref().unwrap_or("any");
        sub_ctx["step"] = json!(step);
    }

    if sf.field_type == FieldType::Textarea {
        let rows = sf.admin.rows.unwrap_or(8);
        sub_ctx["rows"] = json!(rows);
    }

    if sf.field_type == FieldType::Date {
        if let Some(ref md) = sf.min_date {
            sub_ctx["min_date"] = json!(md);
        }

        if let Some(ref md) = sf.max_date {
            sub_ctx["max_date"] = json!(md);
        }
    }

    if depth >= MAX_FIELD_DEPTH {
        return;
    }

    match &sf.field_type {
        FieldType::Checkbox => {
            let checked = matches!(value, "1" | "true" | "on" | "yes");
            sub_ctx["checked"] = json!(checked);
        }
        FieldType::Select | FieldType::Radio => {
            if sf.has_many {
                let selected_values: HashSet<String> = from_str(value).unwrap_or_default();

                let options: Vec<_> = sf
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

                sub_ctx["options"] = json!(options);
                sub_ctx["has_many"] = json!(true);
            } else {
                let options: Vec<_> = sf
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

                sub_ctx["options"] = json!(options);
            }
        }
        FieldType::Date => {
            let appearance = sf.picker_appearance.as_deref().unwrap_or("dayOnly");

            sub_ctx["picker_appearance"] = json!(appearance);

            match appearance {
                "dayOnly" => {
                    let date_val = if value.len() >= 10 {
                        &value[..10]
                    } else {
                        value
                    };
                    sub_ctx["date_only_value"] = json!(date_val);
                }
                "dayAndTime" => {
                    let dt_val = if value.len() >= 16 {
                        &value[..16]
                    } else {
                        value
                    };
                    sub_ctx["datetime_local_value"] = json!(dt_val);
                }
                _ => {}
            }
        }
        FieldType::Array => {
            let template_prefix = format!("{}[__INDEX__]", name_prefix);
            let sub_fields: Vec<_> = sf
                .fields
                .iter()
                .map(|nested| {
                    build_single_field_context(
                        nested,
                        &HashMap::new(),
                        &HashMap::new(),
                        &template_prefix,
                        non_default_locale,
                        depth + 1,
                    )
                })
                .collect();

            sub_ctx["sub_fields"] = json!(sub_fields);
            sub_ctx["row_count"] = json!(0);
            sub_ctx["template_id"] = json!(safe_template_id(name_prefix));

            if let Some(ref lf) = sf.admin.label_field {
                sub_ctx["label_field"] = json!(lf);
            }

            if let Some(max) = sf.max_rows {
                sub_ctx["max_rows"] = json!(max);
            }

            if let Some(min) = sf.min_rows {
                sub_ctx["min_rows"] = json!(min);
            }

            sub_ctx["init_collapsed"] = json!(sf.admin.collapsed);

            if let Some(ref ls) = sf.admin.labels_singular {
                sub_ctx["add_label"] = json!(ls.resolve_default());
            }
        }
        FieldType::Group => {
            let sub_fields: Vec<_> = sf
                .fields
                .iter()
                .map(|nested| {
                    build_single_field_context(
                        nested,
                        values,
                        errors,
                        name_prefix,
                        non_default_locale,
                        depth + 1,
                    )
                })
                .collect();

            sub_ctx["sub_fields"] = json!(sub_fields);
            sub_ctx["collapsed"] = json!(sf.admin.collapsed);
        }
        FieldType::Row => {
            let sub_fields: Vec<_> = sf
                .fields
                .iter()
                .map(|nested| {
                    build_single_field_context(
                        nested,
                        values,
                        errors,
                        name_prefix,
                        non_default_locale,
                        depth + 1,
                    )
                })
                .collect();

            sub_ctx["sub_fields"] = json!(sub_fields);
        }
        FieldType::Collapsible => {
            let sub_fields: Vec<_> = sf
                .fields
                .iter()
                .map(|nested| {
                    build_single_field_context(
                        nested,
                        values,
                        errors,
                        name_prefix,
                        non_default_locale,
                        depth + 1,
                    )
                })
                .collect();

            sub_ctx["sub_fields"] = json!(sub_fields);
            sub_ctx["collapsed"] = json!(sf.admin.collapsed);
        }
        FieldType::Tabs => {
            let tabs_ctx: Vec<_> = sf
                .tabs
                .iter()
                .map(|tab| {
                    let tab_sub_fields: Vec<_> = tab
                        .fields
                        .iter()
                        .map(|nested| {
                            build_single_field_context(
                                nested,
                                values,
                                errors,
                                name_prefix,
                                non_default_locale,
                                depth + 1,
                            )
                        })
                        .collect();

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

            sub_ctx["tabs"] = json!(tabs_ctx);
        }
        FieldType::Blocks => {
            let block_defs: Vec<_> = sf.blocks.iter().map(|bd| {
                let template_prefix = format!("{}[__INDEX__]", name_prefix);
                let block_fields: Vec<_> = bd.fields.iter().map(|nested| {
                    build_single_field_context(nested, &HashMap::new(), &HashMap::new(), &template_prefix, non_default_locale, depth + 1)
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

            sub_ctx["block_definitions"] = json!(block_defs);
            sub_ctx["row_count"] = json!(0);
            sub_ctx["template_id"] = json!(safe_template_id(name_prefix));

            if let Some(max) = sf.max_rows {
                sub_ctx["max_rows"] = json!(max);
            }

            if let Some(min) = sf.min_rows {
                sub_ctx["min_rows"] = json!(min);
            }

            sub_ctx["init_collapsed"] = json!(sf.admin.collapsed);

            if let Some(ref ls) = sf.admin.labels_singular {
                sub_ctx["add_label"] = json!(ls.resolve_default());
            }

            if let Some(ref p) = sf.admin.picker {
                sub_ctx["picker"] = json!(p);
            }
        }
        FieldType::Relationship => {
            if let Some(ref rc) = sf.relationship {
                sub_ctx["relationship_collection"] = json!(rc.collection);
                sub_ctx["has_many"] = json!(rc.has_many);

                if rc.is_polymorphic() {
                    sub_ctx["polymorphic"] = json!(true);
                    sub_ctx["collections"] = json!(rc.polymorphic);
                }
            }

            if let Some(ref p) = sf.admin.picker {
                sub_ctx["picker"] = json!(p);
            }
        }
        FieldType::Upload => {
            if let Some(ref rc) = sf.relationship {
                sub_ctx["relationship_collection"] = json!(rc.collection);

                if rc.has_many {
                    sub_ctx["has_many"] = json!(true);
                }
            }

            let picker = sf.admin.picker.as_deref().unwrap_or("drawer");
            if picker != "none" {
                sub_ctx["picker"] = json!(picker);
            }
        }
        FieldType::Code => {
            let lang = sf.admin.language.as_deref().unwrap_or("json");
            sub_ctx["language"] = json!(lang);
        }
        FieldType::Text | FieldType::Number if sf.has_many => {
            let tags: Vec<String> = from_str(value).unwrap_or_default();
            sub_ctx["has_many"] = json!(true);
            sub_ctx["tags"] = json!(tags);
            sub_ctx["value"] = json!(tags.join(","));
        }
        _ => {}
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
mod tests;
