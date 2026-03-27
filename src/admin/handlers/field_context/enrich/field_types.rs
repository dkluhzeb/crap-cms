//! Sub-field context enrichment helpers called from the dispatch loop in mod.rs.
//!
//! Top-level enrichment helpers that require DB access live in `enrich_types.rs`.

use super::{
    children::build_enriched_children_from_data, nested::build_enriched_sub_field_context,
};
use crate::{
    admin::handlers::{
        field_context::{
            builder::{apply_field_type_extras, build_single_field_context},
            count_errors_in_fields, safe_template_id,
        },
        shared::auto_label_from_name,
    },
    core::field::{FieldDefinition, FieldType},
};

use std::collections::{HashMap, HashSet};

use serde_json::{Value, from_str, json};

// ── build_enriched_sub_field_context helpers ─────────────────────────

/// Enrich a Checkbox sub-field context.
pub(super) fn sub_checkbox(sub_ctx: &mut Value, val: &str) {
    let checked = matches!(val, "1" | "true" | "on" | "yes");

    sub_ctx["checked"] = json!(checked);
}

/// Enrich a Select/Radio sub-field context.
pub(super) fn sub_select_radio(sub_ctx: &mut Value, sf: &FieldDefinition, val: &str) {
    if sf.has_many {
        let selected_values: HashSet<String> = from_str(val).unwrap_or_default();

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
                    "selected": opt.value == val,
                })
            })
            .collect();

        sub_ctx["options"] = json!(options);
    }
}

/// Enrich a Date sub-field context.
pub(super) fn sub_date(sub_ctx: &mut Value, sf: &FieldDefinition, val: &str, tz_value: &str) {
    let appearance = sf.picker_appearance.as_deref().unwrap_or("dayOnly");

    sub_ctx["picker_appearance"] = json!(appearance);

    // Convert UTC back to local time for display if timezone is stored
    let display_value = if !tz_value.is_empty() && !val.is_empty() {
        crate::db::query::helpers::utc_to_local(val, tz_value).unwrap_or_else(|| val.to_string())
    } else {
        val.to_string()
    };

    match appearance {
        "dayOnly" => {
            let date_val = display_value.get(..10).unwrap_or(&display_value);
            sub_ctx["date_only_value"] = json!(date_val);
        }
        "dayAndTime" => {
            let dt_val = display_value.get(..16).unwrap_or(&display_value);
            sub_ctx["datetime_local_value"] = json!(dt_val);
        }
        _ => {}
    }

    crate::admin::handlers::field_context::add_timezone_context(sub_ctx, sf, tz_value, "");
}

/// Enrich a Relationship sub-field context.
pub(super) fn sub_relationship(sub_ctx: &mut Value, sf: &FieldDefinition) {
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

/// Enrich an Upload sub-field context.
pub(super) fn sub_upload(sub_ctx: &mut Value, sf: &FieldDefinition) {
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

/// Enrich a nested Array sub-field context (within another Array/Blocks row).
pub(super) fn sub_array(
    sub_ctx: &mut Value,
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &super::SubFieldOpts,
) {
    let nested_opts = super::SubFieldOpts::builder(opts.errors)
        .locale_locked(opts.locale_locked)
        .non_default_locale(opts.non_default_locale)
        .depth(opts.depth + 1)
        .build();

    let nested_rows: Vec<Value> = match raw_value {
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(nested_idx, nested_row)| {
                let nested_row_obj = nested_row.as_object();
                let mut nested_sub_values: Vec<_> = sf
                    .fields
                    .iter()
                    .map(|nested_sf| {
                        let nested_raw = if matches!(
                            nested_sf.field_type,
                            FieldType::Tabs | FieldType::Row | FieldType::Collapsible
                        ) {
                            Some(nested_row)
                        } else {
                            nested_row_obj.and_then(|m| m.get(&nested_sf.name))
                        };

                        build_enriched_sub_field_context(
                            nested_sf,
                            nested_raw,
                            indexed_name,
                            nested_idx,
                            &nested_opts,
                        )
                    })
                    .collect();

                crate::admin::handlers::field_context::inject_timezone_values_from_row(
                    &mut nested_sub_values,
                    &sf.fields,
                    nested_row_obj,
                );

                let row_has_errors = nested_sub_values
                    .iter()
                    .any(|sf_ctx| sf_ctx.get("error").is_some());

                let mut row_json = json!({
                    "index": nested_idx,
                    "sub_fields": nested_sub_values,
                });

                if row_has_errors {
                    row_json["has_errors"] = json!(true);
                }

                row_json
            })
            .collect(),
        _ => Vec::new(),
    };

    let template_prefix = format!("{}[__INDEX__]", indexed_name);

    let template_sub_fields: Vec<_> = sf
        .fields
        .iter()
        .map(|nested_sf| {
            build_single_field_context(
                nested_sf,
                &HashMap::new(),
                &HashMap::new(),
                &template_prefix,
                opts.non_default_locale,
                opts.depth + 1,
            )
        })
        .collect();
    sub_ctx["sub_fields"] = json!(template_sub_fields);
    sub_ctx["rows"] = json!(nested_rows);
    sub_ctx["row_count"] = json!(nested_rows.len());
    sub_ctx["template_id"] = json!(safe_template_id(indexed_name));

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

/// Enrich a nested Blocks sub-field context (within another Array/Blocks row).
pub(super) fn sub_blocks(
    sub_ctx: &mut Value,
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &super::SubFieldOpts,
) {
    let nested_opts = super::SubFieldOpts::builder(opts.errors)
        .locale_locked(opts.locale_locked)
        .non_default_locale(opts.non_default_locale)
        .depth(opts.depth + 1)
        .build();

    let nested_rows: Vec<Value> = match raw_value {
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(nested_idx, nested_row)| {
                let nested_row_obj = nested_row.as_object();

                let block_type = nested_row_obj
                    .and_then(|m| m.get("_block_type"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                let block_label = sf
                    .blocks
                    .iter()
                    .find(|bd| bd.block_type == block_type)
                    .and_then(|bd| bd.label.as_ref().map(|ls| ls.resolve_default()))
                    .unwrap_or(block_type);

                let block_def = sf.blocks.iter().find(|bd| bd.block_type == block_type);

                let mut nested_sub_values: Vec<_> = block_def
                    .map(|bd| {
                        bd.fields
                            .iter()
                            .map(|nested_sf| {
                                let nested_raw = if matches!(
                                    nested_sf.field_type,
                                    FieldType::Tabs | FieldType::Row | FieldType::Collapsible
                                ) {
                                    Some(nested_row)
                                } else {
                                    nested_row_obj.and_then(|m| m.get(&nested_sf.name))
                                };

                                build_enriched_sub_field_context(
                                    nested_sf,
                                    nested_raw,
                                    indexed_name,
                                    nested_idx,
                                    &nested_opts,
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                if let Some(bd) = block_def {
                    crate::admin::handlers::field_context::inject_timezone_values_from_row(
                        &mut nested_sub_values,
                        &bd.fields,
                        nested_row_obj,
                    );
                }

                let row_has_errors = nested_sub_values
                    .iter()
                    .any(|sf_ctx| sf_ctx.get("error").is_some());

                let mut row_json = json!({
                    "index": nested_idx,
                    "_block_type": block_type,
                    "block_label": block_label,
                    "sub_fields": nested_sub_values,
                });

                if row_has_errors {
                    row_json["has_errors"] = json!(true);
                }

                row_json
            })
            .collect(),
        _ => Vec::new(),
    };

    let block_defs: Vec<_> = sf
        .blocks
        .iter()
        .map(|bd| {
            let template_prefix = format!("{}[__INDEX__]", indexed_name);

            let block_fields: Vec<_> = bd
                .fields
                .iter()
                .map(|nested_sf| {
                    build_single_field_context(
                        nested_sf,
                        &HashMap::new(),
                        &HashMap::new(),
                        &template_prefix,
                        opts.non_default_locale,
                        opts.depth + 1,
                    )
                })
                .collect();

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
        })
        .collect();

    sub_ctx["block_definitions"] = json!(block_defs);
    sub_ctx["rows"] = json!(nested_rows);
    sub_ctx["row_count"] = json!(nested_rows.len());
    sub_ctx["template_id"] = json!(safe_template_id(indexed_name));

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

/// Enrich a nested Group sub-field context.
///
/// Group inside Array/Blocks uses `[0]` index notation to match the form parser's
/// convention (Group is treated as a single-element composite). For example,
/// `items[0][meta][0][title]` for field "title" inside Group "meta" inside Array "items".
pub(super) fn sub_group(
    sub_ctx: &mut Value,
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &super::SubFieldOpts,
) {
    let locale_locked = opts.locale_locked;
    let non_default_locale = opts.non_default_locale;
    let depth = opts.depth;
    let errors = opts.errors;

    let group_obj = match raw_value {
        Some(Value::Object(_)) => raw_value,
        _ => None,
    };

    let nested_sub_fields: Vec<_> = sf
        .fields
        .iter()
        .map(|nested_sf| {
            let nested_raw = group_obj
                .and_then(|v| v.as_object())
                .and_then(|m| m.get(&nested_sf.name));

            // Use [0] index for Group children: items[0][meta][0][title]
            let is_wrapper = matches!(
                nested_sf.field_type,
                FieldType::Tabs | FieldType::Row | FieldType::Collapsible
            );
            let nested_name = if is_wrapper {
                format!("{}[0]", indexed_name)
            } else {
                format!("{}[0][{}]", indexed_name, nested_sf.name)
            };

            let nested_val = nested_raw
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    Value::Null => String::new(),
                    other => match nested_sf.field_type {
                        FieldType::Array
                        | FieldType::Blocks
                        | FieldType::Group
                        | FieldType::Row
                        | FieldType::Collapsible
                        | FieldType::Tabs => String::new(),
                        _ => other.to_string(),
                    },
                })
                .unwrap_or_default();

            let nested_label = nested_sf
                .admin
                .label
                .as_ref()
                .map(|ls| ls.resolve_default().to_string())
                .unwrap_or_else(|| auto_label_from_name(&nested_sf.name));

            let mut nested_ctx = json!({
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

            if let Some(err) = errors.get(&nested_name) {
                nested_ctx["error"] = json!(err);
            }

            if depth >= super::super::MAX_FIELD_DEPTH {
                return nested_ctx;
            }

            // Dispatch composite children to their respective enrichment functions
            match nested_sf.field_type {
                FieldType::Group => {
                    sub_group(&mut nested_ctx, nested_sf, nested_raw, &nested_name, opts);
                }
                FieldType::Row | FieldType::Collapsible => {
                    sub_row_collapsible(&mut nested_ctx, nested_sf, nested_raw, &nested_name, opts);
                }
                FieldType::Tabs => {
                    sub_tabs(&mut nested_ctx, nested_sf, nested_raw, &nested_name, opts);
                }
                FieldType::Array => {
                    sub_array(&mut nested_ctx, nested_sf, nested_raw, &nested_name, opts);
                }
                FieldType::Blocks => {
                    sub_blocks(&mut nested_ctx, nested_sf, nested_raw, &nested_name, opts);
                }
                _ => {
                    let empty_vals = HashMap::new();
                    let extras_ctx =
                        crate::admin::handlers::field_context::builder::FieldRecursionCtx::builder(
                            &empty_vals,
                            errors,
                            &nested_name,
                        )
                        .non_default_locale(non_default_locale)
                        .depth(depth + 1)
                        .build();
                    apply_field_type_extras(nested_sf, &nested_val, &mut nested_ctx, &extras_ctx);

                    // Inject stored timezone value from parent group data for Date fields
                    if nested_sf.field_type == FieldType::Date && nested_sf.timezone {
                        let tz_key = format!("{}_tz", nested_sf.name);
                        let tz_val = group_obj
                            .and_then(|v| v.as_object())
                            .and_then(|m| m.get(&tz_key))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        if !tz_val.is_empty() {
                            nested_ctx["timezone_value"] = json!(tz_val);
                        }
                    }
                }
            }

            nested_ctx
        })
        .collect();

    sub_ctx["sub_fields"] = json!(nested_sub_fields);
    sub_ctx["collapsed"] = json!(sf.admin.collapsed);
}

/// Enrich a nested Row/Collapsible sub-field context.
pub(super) fn sub_row_collapsible(
    sub_ctx: &mut Value,
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &super::SubFieldOpts,
) {
    let nested_sub_fields = build_enriched_children_from_data(
        &sf.fields,
        raw_value,
        indexed_name,
        opts.locale_locked,
        opts.non_default_locale,
        opts.depth + 1,
        opts.errors,
    );

    sub_ctx["sub_fields"] = json!(nested_sub_fields);

    if sf.field_type == FieldType::Collapsible {
        sub_ctx["collapsed"] = json!(sf.admin.collapsed);
    }
}

/// Enrich a nested Tabs sub-field context.
pub(super) fn sub_tabs(
    sub_ctx: &mut Value,
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &super::SubFieldOpts,
) {
    let tabs_ctx: Vec<_> = sf
        .tabs
        .iter()
        .map(|tab| {
            let tab_sub_fields = build_enriched_children_from_data(
                &tab.fields,
                raw_value,
                indexed_name,
                opts.locale_locked,
                opts.non_default_locale,
                opts.depth + 1,
                opts.errors,
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

    sub_ctx["tabs"] = json!(tabs_ctx);
}

/// Enrich a Text/Number has_many sub-field context (tag input).
pub(super) fn sub_has_many_tags(sub_ctx: &mut Value, val: &str) {
    let tags: Vec<String> = from_str(val).unwrap_or_default();
    sub_ctx["has_many"] = json!(true);
    sub_ctx["tags"] = json!(tags);
    sub_ctx["value"] = json!(tags.join(","));
}
