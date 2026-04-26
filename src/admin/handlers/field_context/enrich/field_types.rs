//! Sub-field context enrichment helpers called from the dispatch loop in mod.rs.
//!
//! Top-level enrichment helpers that require DB access live in `enrich_types.rs`.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, from_str, json};

use crate::{
    admin::handlers::{
        field_context::{
            add_timezone_context,
            builder::{FieldRecursionCtx, apply_field_type_extras, build_single_field_context},
            count_errors_in_fields,
            enrich::{
                SubFieldOpts, children::build_enriched_children_from_data,
                nested::build_enriched_sub_field_context,
            },
            inject_lang_values_from_row, inject_timezone_values_from_row, safe_template_id,
        },
        shared::auto_label_from_name,
    },
    core::field::{FieldDefinition, FieldType},
    db::query::helpers::utc_to_local,
};

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
        utc_to_local(val, tz_value).unwrap_or_else(|| val.to_string())
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

    add_timezone_context(sub_ctx, sf, tz_value, "");
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

/// Extract the raw value for a nested sub-field, handling layout wrappers transparently.
fn extract_nested_raw<'a>(
    nested_sf: &FieldDefinition,
    row: &'a Value,
    row_obj: Option<&'a serde_json::Map<String, Value>>,
) -> Option<&'a Value> {
    if matches!(
        nested_sf.field_type,
        FieldType::Tabs | FieldType::Row | FieldType::Collapsible
    ) {
        Some(row)
    } else {
        row_obj.and_then(|m| m.get(&nested_sf.name))
    }
}

/// Build a single nested array row with sub-fields and error flag.
fn build_nested_array_row(
    sf: &FieldDefinition,
    nested_row: &Value,
    nested_idx: usize,
    indexed_name: &str,
    nested_opts: &SubFieldOpts,
) -> Value {
    let nested_row_obj = nested_row.as_object();

    let mut sub_values: Vec<_> = sf
        .fields
        .iter()
        .map(|nested_sf| {
            let nested_raw = extract_nested_raw(nested_sf, nested_row, nested_row_obj);
            build_enriched_sub_field_context(
                nested_sf,
                nested_raw,
                indexed_name,
                nested_idx,
                nested_opts,
            )
        })
        .collect();

    inject_timezone_values_from_row(&mut sub_values, &sf.fields, nested_row_obj);
    inject_lang_values_from_row(&mut sub_values, &sf.fields, nested_row_obj);

    let row_has_errors = sub_values
        .iter()
        .any(|sf_ctx| sf_ctx.get("error").is_some());

    let mut row_json = json!({
        "index": nested_idx,
        "sub_fields": sub_values,
    });

    if row_has_errors {
        row_json["has_errors"] = json!(true);
    }

    row_json
}

/// Build template sub-fields for a nested array (used for JS-added rows).
fn build_nested_template_sub_fields(
    sf: &FieldDefinition,
    indexed_name: &str,
    opts: &SubFieldOpts,
) -> Vec<Value> {
    let template_prefix = format!("{}[__INDEX__]", indexed_name);

    sf.fields
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
        .collect()
}

/// Apply shared array/blocks metadata (row limits, collapse, add label) to a sub-field context.
fn apply_row_metadata(sub_ctx: &mut Value, sf: &FieldDefinition, indexed_name: &str) {
    sub_ctx["template_id"] = json!(safe_template_id(indexed_name));
    sub_ctx["init_collapsed"] = json!(sf.admin.collapsed);

    if let Some(max) = sf.max_rows {
        sub_ctx["max_rows"] = json!(max);
    }

    if let Some(min) = sf.min_rows {
        sub_ctx["min_rows"] = json!(min);
    }

    if let Some(ref ls) = sf.admin.labels_singular {
        sub_ctx["add_label"] = json!(ls.resolve_default());
    }
}

/// Enrich a nested Array sub-field context (within another Array/Blocks row).
pub(super) fn sub_array(
    sub_ctx: &mut Value,
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &SubFieldOpts,
) {
    let nested_opts = SubFieldOpts::builder(opts.errors)
        .locale_locked(opts.locale_locked)
        .non_default_locale(opts.non_default_locale)
        .depth(opts.depth + 1)
        .build();

    let nested_rows: Vec<Value> = match raw_value {
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(idx, row)| build_nested_array_row(sf, row, idx, indexed_name, &nested_opts))
            .collect(),
        _ => Vec::new(),
    };

    sub_ctx["sub_fields"] = json!(build_nested_template_sub_fields(sf, indexed_name, opts));
    sub_ctx["rows"] = json!(nested_rows);
    sub_ctx["row_count"] = json!(nested_rows.len());

    apply_row_metadata(sub_ctx, sf, indexed_name);
}

/// Build a single nested blocks row with sub-fields, block type, and error flag.
fn build_nested_blocks_row(
    sf: &FieldDefinition,
    nested_row: &Value,
    nested_idx: usize,
    indexed_name: &str,
    nested_opts: &SubFieldOpts,
) -> Value {
    let nested_row_obj = nested_row.as_object();

    let block_type = nested_row_obj
        .and_then(|m| m.get("_block_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let block_def = sf.blocks.iter().find(|bd| bd.block_type == block_type);

    let block_label = block_def
        .and_then(|bd| bd.label.as_ref().map(|ls| ls.resolve_default()))
        .unwrap_or(block_type);

    let mut sub_values: Vec<_> = block_def
        .map(|bd| {
            bd.fields
                .iter()
                .map(|nested_sf| {
                    let nested_raw = extract_nested_raw(nested_sf, nested_row, nested_row_obj);
                    build_enriched_sub_field_context(
                        nested_sf,
                        nested_raw,
                        indexed_name,
                        nested_idx,
                        nested_opts,
                    )
                })
                .collect()
        })
        .unwrap_or_default();

    if let Some(bd) = block_def {
        inject_timezone_values_from_row(&mut sub_values, &bd.fields, nested_row_obj);
        inject_lang_values_from_row(&mut sub_values, &bd.fields, nested_row_obj);
    }

    let row_has_errors = sub_values
        .iter()
        .any(|sf_ctx| sf_ctx.get("error").is_some());

    let mut row_json = json!({
        "index": nested_idx,
        "_block_type": block_type,
        "block_label": block_label,
        "sub_fields": sub_values,
    });

    if row_has_errors {
        row_json["has_errors"] = json!(true);
    }

    row_json
}

/// Build a single block definition JSON for the template.
fn build_block_def_template(
    bd: &crate::core::field::BlockDefinition,
    indexed_name: &str,
    opts: &SubFieldOpts,
) -> Value {
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
}

/// Enrich a nested Blocks sub-field context (within another Array/Blocks row).
pub(super) fn sub_blocks(
    sub_ctx: &mut Value,
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &SubFieldOpts,
) {
    let nested_opts = SubFieldOpts::builder(opts.errors)
        .locale_locked(opts.locale_locked)
        .non_default_locale(opts.non_default_locale)
        .depth(opts.depth + 1)
        .build();

    let nested_rows: Vec<Value> = match raw_value {
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(idx, row)| build_nested_blocks_row(sf, row, idx, indexed_name, &nested_opts))
            .collect(),
        _ => Vec::new(),
    };

    let block_defs: Vec<_> = sf
        .blocks
        .iter()
        .map(|bd| build_block_def_template(bd, indexed_name, opts))
        .collect();

    sub_ctx["block_definitions"] = json!(block_defs);
    sub_ctx["rows"] = json!(nested_rows);
    sub_ctx["row_count"] = json!(nested_rows.len());

    if let Some(ref lf) = sf.admin.label_field {
        sub_ctx["label_field"] = json!(lf);
    }

    apply_row_metadata(sub_ctx, sf, indexed_name);
}

/// Resolve the form name for a group child field.
///
/// Layout wrappers get the `[0]` suffix only, leaf fields get `[0][field_name]`.
fn group_child_name(indexed_name: &str, nested_sf: &FieldDefinition) -> String {
    let is_wrapper = matches!(
        nested_sf.field_type,
        FieldType::Tabs | FieldType::Row | FieldType::Collapsible
    );

    if is_wrapper {
        format!("{}[0]", indexed_name)
    } else {
        format!("{}[0][{}]", indexed_name, nested_sf.name)
    }
}

/// Convert a raw JSON value to a string for a group child field.
fn group_child_value(nested_raw: Option<&Value>, nested_sf: &FieldDefinition) -> String {
    nested_raw
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
        .unwrap_or_default()
}

/// Build the base context for a single group child field.
fn build_group_child_base(
    nested_sf: &FieldDefinition,
    nested_name: &str,
    nested_val: &str,
    opts: &SubFieldOpts,
) -> Value {
    let nested_label = nested_sf
        .admin
        .label
        .as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| auto_label_from_name(&nested_sf.name));

    let mut ctx = json!({
        "name": nested_name,
        "field_type": nested_sf.field_type.as_str(),
        "label": nested_label,
        "value": nested_val,
        "required": nested_sf.required,
        "readonly": nested_sf.admin.readonly || opts.locale_locked,
        "locale_locked": opts.locale_locked,
        "placeholder": nested_sf.admin.placeholder.as_ref().map(|ls| ls.resolve_default()),
        "description": nested_sf.admin.description.as_ref().map(|ls| ls.resolve_default()),
    });

    if let Some(err) = opts.errors.get(nested_name) {
        ctx["error"] = json!(err);
    }

    ctx
}

/// Dispatch type-specific enrichment for a group child field.
fn enrich_group_child(
    nested_ctx: &mut Value,
    nested_sf: &FieldDefinition,
    nested_raw: Option<&Value>,
    nested_name: &str,
    nested_val: &str,
    group_obj: Option<&Value>,
    opts: &SubFieldOpts,
) {
    match nested_sf.field_type {
        FieldType::Group => {
            sub_group(nested_ctx, nested_sf, nested_raw, nested_name, opts);
        }
        FieldType::Row | FieldType::Collapsible => {
            sub_row_collapsible(nested_ctx, nested_sf, nested_raw, nested_name, opts);
        }
        FieldType::Tabs => {
            sub_tabs(nested_ctx, nested_sf, nested_raw, nested_name, opts);
        }
        FieldType::Array => {
            sub_array(nested_ctx, nested_sf, nested_raw, nested_name, opts);
        }
        FieldType::Blocks => {
            sub_blocks(nested_ctx, nested_sf, nested_raw, nested_name, opts);
        }
        _ => {
            enrich_group_leaf(
                nested_ctx,
                nested_sf,
                nested_val,
                nested_name,
                group_obj,
                opts,
            );
        }
    }
}

/// Apply field_type_extras and timezone injection for a leaf field inside a group.
fn enrich_group_leaf(
    nested_ctx: &mut Value,
    nested_sf: &FieldDefinition,
    nested_val: &str,
    nested_name: &str,
    group_obj: Option<&Value>,
    opts: &SubFieldOpts,
) {
    let empty_vals = HashMap::new();
    let extras_ctx = FieldRecursionCtx::builder(&empty_vals, opts.errors, nested_name)
        .non_default_locale(opts.non_default_locale)
        .depth(opts.depth + 1)
        .build();

    apply_field_type_extras(nested_sf, nested_val, nested_ctx, &extras_ctx);

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
    opts: &SubFieldOpts,
) {
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

            let nested_name = group_child_name(indexed_name, nested_sf);
            let nested_val = group_child_value(nested_raw, nested_sf);
            let mut nested_ctx = build_group_child_base(nested_sf, &nested_name, &nested_val, opts);

            if opts.depth < super::super::MAX_FIELD_DEPTH {
                enrich_group_child(
                    &mut nested_ctx,
                    nested_sf,
                    nested_raw,
                    &nested_name,
                    &nested_val,
                    group_obj,
                    opts,
                );
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
    opts: &SubFieldOpts,
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

/// Build a single tab context with sub_fields and error count.
fn build_tab_context(
    tab: &crate::core::field::FieldTab,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &SubFieldOpts,
) -> Value {
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
}

/// Enrich a nested Tabs sub-field context.
pub(super) fn sub_tabs(
    sub_ctx: &mut Value,
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &SubFieldOpts,
) {
    let tabs_ctx: Vec<_> = sf
        .tabs
        .iter()
        .map(|tab| build_tab_context(tab, raw_value, indexed_name, opts))
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
