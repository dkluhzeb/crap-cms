//! Sub-field context enrichment helpers called from the dispatch loop in mod.rs.
//!
//! Top-level enrichment helpers that require DB access live in `enrich_types.rs`.

use std::collections::{HashMap, HashSet};

use serde_json::{Value, from_str};

use crate::{
    admin::{
        context::field::{
            ArrayField, ArrayRow, BlockRow, BlocksField, CheckboxField, ChoiceField, DateField,
            FieldContext, GroupField, NumberField, RelationshipField, RowField, SelectOption,
            TabPanel, TabsField, TextField, TimezoneOption, UploadField,
        },
        handlers::{
            field_context::{
                builder::build_single_field_context,
                count_errors_in_field_contexts,
                enrich::{
                    SubFieldOpts, children::build_enriched_children_from_data,
                    nested::build_enriched_sub_field_context,
                },
                inject_lang_values_from_row, inject_timezone_values_from_row, safe_template_id,
            },
            shared::auto_label_from_name,
        },
    },
    core::{
        field::{FieldDefinition, FieldType},
        timezone::TIMEZONE_OPTIONS,
    },
    db::query::helpers::utc_to_local,
};

// ── build_enriched_sub_field_context helpers ─────────────────────────

/// Enrich a Checkbox sub-field context.
pub(super) fn sub_checkbox(cf: &mut CheckboxField, val: &str) {
    cf.checked = matches!(val, "1" | "true" | "on" | "yes");
}

/// Enrich a Select/Radio sub-field context.
pub(super) fn sub_select_radio(cf: &mut ChoiceField, sf: &FieldDefinition, val: &str) {
    if sf.has_many {
        let selected_values: HashSet<String> = from_str(val).unwrap_or_default();

        cf.options = sf
            .options
            .iter()
            .map(|opt| SelectOption {
                label: opt.label.resolve_default().to_string(),
                value: opt.value.clone(),
                selected: selected_values.contains(&opt.value),
            })
            .collect();
        cf.has_many = Some(true);
    } else {
        cf.options = sf
            .options
            .iter()
            .map(|opt| SelectOption {
                label: opt.label.resolve_default().to_string(),
                value: opt.value.clone(),
                selected: opt.value == val,
            })
            .collect();
    }
}

/// Enrich a Date sub-field context.
pub(super) fn sub_date(df: &mut DateField, sf: &FieldDefinition, val: &str, tz_value: &str) {
    let appearance = sf.picker_appearance.as_deref().unwrap_or("dayOnly");
    df.picker_appearance = appearance.to_string();

    // Convert UTC back to local time for display if timezone is stored
    let display_value = if !tz_value.is_empty() && !val.is_empty() {
        utc_to_local(val, tz_value).unwrap_or_else(|| val.to_string())
    } else {
        val.to_string()
    };

    match appearance {
        "dayOnly" => {
            df.date_only_value = Some(
                display_value
                    .get(..10)
                    .unwrap_or(&display_value)
                    .to_string(),
            );
        }
        "dayAndTime" => {
            df.datetime_local_value = Some(
                display_value
                    .get(..16)
                    .unwrap_or(&display_value)
                    .to_string(),
            );
        }
        _ => {}
    }

    if sf.timezone {
        let default_tz = sf
            .default_timezone
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("");
        df.timezone_enabled = Some(true);
        df.default_timezone = Some(default_tz.to_string());
        df.timezone_options = Some(
            TIMEZONE_OPTIONS
                .iter()
                .map(|(code, label)| TimezoneOption {
                    value: (*code).to_string(),
                    label: (*label).to_string(),
                })
                .collect(),
        );
        df.timezone_value = Some(tz_value.to_string());
    }
}

/// Enrich a Relationship sub-field context.
pub(super) fn sub_relationship(rf: &mut RelationshipField, sf: &FieldDefinition) {
    if let Some(ref rc) = sf.relationship {
        rf.relationship_collection = Some(rc.collection.to_string());
        rf.has_many = Some(rc.has_many);

        if rc.is_polymorphic() {
            rf.polymorphic = Some(true);
            rf.collections = Some(rc.polymorphic.iter().map(ToString::to_string).collect());
        }
    }

    if let Some(ref p) = sf.admin.picker {
        rf.picker = Some(p.clone());
    }
}

/// Enrich an Upload sub-field context.
pub(super) fn sub_upload(uf: &mut UploadField, sf: &FieldDefinition) {
    if let Some(ref rc) = sf.relationship {
        uf.relationship_collection = Some(rc.collection.to_string());
        if rc.has_many {
            uf.has_many = Some(true);
        }
    }

    let picker = sf.admin.picker.as_deref().unwrap_or("drawer");
    if picker != "none" {
        uf.picker = Some(picker.to_string());
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

/// Build a single typed [`ArrayRow`] with sub-fields and error flag.
fn build_nested_array_row(
    sf: &FieldDefinition,
    nested_row: &Value,
    nested_idx: usize,
    indexed_name: &str,
    nested_opts: &SubFieldOpts,
) -> ArrayRow {
    let nested_row_obj = nested_row.as_object();

    let mut sub_fields: Vec<FieldContext> = sf
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

    inject_timezone_values_from_row(&mut sub_fields, &sf.fields, nested_row_obj);
    inject_lang_values_from_row(&mut sub_fields, &sf.fields, nested_row_obj);

    let row_has_errors = sub_fields.iter().any(|fc| fc.base().error.is_some());

    ArrayRow {
        index: nested_idx,
        sub_fields,
        has_errors: if row_has_errors { Some(true) } else { None },
        custom_label: None,
    }
}

/// Build template sub-fields for a nested array (used for JS-added rows).
fn build_nested_template_sub_fields(
    sf: &FieldDefinition,
    indexed_name: &str,
    opts: &SubFieldOpts,
) -> Vec<FieldContext> {
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

/// Apply shared array metadata (row limits, collapse, add label) to a typed array variant.
fn apply_array_row_metadata(af: &mut ArrayField, sf: &FieldDefinition, indexed_name: &str) {
    af.template_id = safe_template_id(indexed_name);
    af.init_collapsed = sf.admin.collapsed;
    af.max_rows = sf.max_rows;
    af.min_rows = sf.min_rows;
    af.add_label = sf
        .admin
        .labels_singular
        .as_ref()
        .map(|ls| ls.resolve_default().to_string());
}

/// Apply shared blocks metadata (row limits, collapse, add label) to a typed blocks variant.
fn apply_blocks_row_metadata(bf: &mut BlocksField, sf: &FieldDefinition, indexed_name: &str) {
    bf.template_id = safe_template_id(indexed_name);
    bf.init_collapsed = sf.admin.collapsed;
    bf.max_rows = sf.max_rows;
    bf.min_rows = sf.min_rows;
    bf.add_label = sf
        .admin
        .labels_singular
        .as_ref()
        .map(|ls| ls.resolve_default().to_string());
}

/// Enrich a nested Array sub-field context (within another Array/Blocks row).
pub(super) fn sub_array(
    af: &mut ArrayField,
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

    let nested_rows: Vec<ArrayRow> = match raw_value {
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(idx, row)| build_nested_array_row(sf, row, idx, indexed_name, &nested_opts))
            .collect(),
        _ => Vec::new(),
    };

    af.sub_fields = build_nested_template_sub_fields(sf, indexed_name, opts);
    af.row_count = nested_rows.len();
    af.rows = Some(nested_rows);
    af.label_field = sf.admin.label_field.clone();

    apply_array_row_metadata(af, sf, indexed_name);
}

/// Build a single typed [`BlockRow`] with sub-fields, block type, and error flag.
fn build_nested_blocks_row(
    sf: &FieldDefinition,
    nested_row: &Value,
    nested_idx: usize,
    indexed_name: &str,
    nested_opts: &SubFieldOpts,
) -> BlockRow {
    let nested_row_obj = nested_row.as_object();

    let block_type = nested_row_obj
        .and_then(|m| m.get("_block_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    let block_def = sf.blocks.iter().find(|bd| bd.block_type == block_type);

    let block_label = block_def
        .and_then(|bd| bd.label.as_ref().map(|ls| ls.resolve_default().to_string()))
        .unwrap_or_else(|| block_type.to_string());

    let mut sub_fields: Vec<FieldContext> = block_def
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
        inject_timezone_values_from_row(&mut sub_fields, &bd.fields, nested_row_obj);
        inject_lang_values_from_row(&mut sub_fields, &bd.fields, nested_row_obj);
    }

    let row_has_errors = sub_fields.iter().any(|fc| fc.base().error.is_some());

    BlockRow {
        index: nested_idx,
        block_type: block_type.to_string(),
        block_label,
        sub_fields,
        has_errors: if row_has_errors { Some(true) } else { None },
        custom_label: None,
    }
}

/// Build a single typed block definition for the template.
fn build_block_def_template(
    bd: &crate::core::field::BlockDefinition,
    indexed_name: &str,
    opts: &SubFieldOpts,
) -> crate::admin::context::field::BlockDefinition {
    let template_prefix = format!("{}[__INDEX__]", indexed_name);

    let block_fields: Vec<FieldContext> = bd
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

    let label = bd
        .label
        .as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| bd.block_type.clone());

    crate::admin::context::field::BlockDefinition {
        block_type: bd.block_type.clone(),
        label,
        fields: block_fields,
        label_field: bd.label_field.clone(),
        group: bd.group.clone(),
        image_url: bd.image_url.clone(),
    }
}

/// Enrich a nested Blocks sub-field context (within another Array/Blocks row).
pub(super) fn sub_blocks(
    bf: &mut BlocksField,
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

    let nested_rows: Vec<BlockRow> = match raw_value {
        Some(Value::Array(arr)) => arr
            .iter()
            .enumerate()
            .map(|(idx, row)| build_nested_blocks_row(sf, row, idx, indexed_name, &nested_opts))
            .collect(),
        _ => Vec::new(),
    };

    bf.block_definitions = sf
        .blocks
        .iter()
        .map(|bd| build_block_def_template(bd, indexed_name, opts))
        .collect();
    bf.row_count = nested_rows.len();
    bf.rows = Some(nested_rows);
    bf.label_field = sf.admin.label_field.clone();

    apply_blocks_row_metadata(bf, sf, indexed_name);
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

/// Build a typed [`FieldContext`] for a single group child field.
///
/// For composite children (Group, Row, Collapsible, Tabs, Array, Blocks),
/// recurse via the corresponding `sub_*` helper to preserve enrichment-
/// specific naming conventions (e.g. Group inside Array uses `[0]` index
/// notation, which differs from the build phase's naming).
///
/// For leaf children (Text, Email, Date, Checkbox, Select, Radio,
/// Relationship, Upload, Code, Richtext, Textarea, Number, Json, Join), use
/// the build phase's typed pipeline directly via
/// [`build_single_field_context`] with a single-entry values map.
fn build_group_child(
    nested_sf: &FieldDefinition,
    nested_raw: Option<&Value>,
    nested_name: &str,
    nested_val: &str,
    group_obj: Option<&Value>,
    opts: &SubFieldOpts,
) -> FieldContext {
    let nested_opts = SubFieldOpts::builder(opts.errors)
        .locale_locked(opts.locale_locked)
        .non_default_locale(opts.non_default_locale)
        .depth(opts.depth + 1)
        .build();

    // Build the typed BaseFieldData mirroring the original
    // `build_group_child_base` shape.
    let nested_label = nested_sf
        .admin
        .label
        .as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| auto_label_from_name(&nested_sf.name));

    let base = crate::admin::context::field::BaseFieldData {
        name: nested_name.to_string(),
        label: nested_label,
        required: nested_sf.required,
        value: Value::String(nested_val.to_string()),
        placeholder: nested_sf
            .admin
            .placeholder
            .as_ref()
            .map(|ls| ls.resolve_default().to_string()),
        description: nested_sf
            .admin
            .description
            .as_ref()
            .map(|ls| ls.resolve_default().to_string()),
        readonly: nested_sf.admin.readonly || opts.locale_locked,
        localized: nested_sf.localized,
        locale_locked: opts.locale_locked,
        position: nested_sf.admin.position.clone(),
        error: opts.errors.get(nested_name).cloned(),
        validation: crate::admin::context::field::ValidationAttrs::default(),
        condition: crate::admin::context::field::ConditionData::default(),
    };

    if opts.depth + 1 >= super::super::MAX_FIELD_DEPTH {
        // Beyond max depth — return a base-only Text variant.
        return FieldContext::Text(TextField {
            base,
            has_many: None,
            tags: None,
        });
    }

    match nested_sf.field_type {
        FieldType::Group => {
            let mut gf = GroupField {
                base,
                sub_fields: Vec::new(),
                collapsed: false,
            };
            sub_group(&mut gf, nested_sf, nested_raw, nested_name, &nested_opts);
            FieldContext::Group(gf)
        }
        FieldType::Row => {
            let mut rf = RowField {
                base,
                sub_fields: Vec::new(),
            };
            sub_row_collapsible_row(&mut rf, nested_sf, nested_raw, nested_name, &nested_opts);
            FieldContext::Row(rf)
        }
        FieldType::Collapsible => {
            let mut gf = GroupField {
                base,
                sub_fields: Vec::new(),
                collapsed: false,
            };
            sub_row_collapsible_group(&mut gf, nested_sf, nested_raw, nested_name, &nested_opts);
            FieldContext::Collapsible(gf)
        }
        FieldType::Tabs => {
            let mut tf = TabsField {
                base,
                tabs: Vec::new(),
            };
            sub_tabs(&mut tf, nested_sf, nested_raw, nested_name, &nested_opts);
            FieldContext::Tabs(tf)
        }
        FieldType::Array => {
            let mut af = ArrayField {
                base,
                sub_fields: Vec::new(),
                rows: None,
                row_count: 0,
                template_id: safe_template_id(nested_name),
                min_rows: None,
                max_rows: None,
                init_collapsed: false,
                add_label: None,
                label_field: None,
            };
            sub_array(&mut af, nested_sf, nested_raw, nested_name, &nested_opts);
            FieldContext::Array(af)
        }
        FieldType::Blocks => {
            let mut bf = BlocksField {
                base,
                block_definitions: Vec::new(),
                rows: None,
                row_count: 0,
                template_id: safe_template_id(nested_name),
                min_rows: None,
                max_rows: None,
                init_collapsed: false,
                add_label: None,
                picker: None,
                label_field: None,
            };
            sub_blocks(&mut bf, nested_sf, nested_raw, nested_name, &nested_opts);
            FieldContext::Blocks(bf)
        }
        _ => {
            // Leaf — use the build phase's typed pipeline. Compose a
            // single-entry values map keyed by the precomputed nested_name
            // so resolve_full_name produces the right full_name.
            let mut values = HashMap::new();
            values.insert(nested_name.to_string(), nested_val.to_string());

            // Date sub-fields with stored timezone need their _tz companion
            // for `single_date`.
            if nested_sf.field_type == FieldType::Date && nested_sf.timezone {
                let tz_key = format!("{}_tz", nested_sf.name);
                if let Some(tz_val) = group_obj
                    .and_then(|v| v.as_object())
                    .and_then(|m| m.get(&tz_key))
                    .and_then(|v| v.as_str())
                    && !tz_val.is_empty()
                {
                    values.insert(format!("{}_tz", nested_name), tz_val.to_string());
                }
            }

            // Synthesize a parent prefix that resolve_full_name turns into
            // the precomputed nested_name. For non-wrapper leaves with a
            // bracketed parent, that's `nested_name` minus the `[field_name]`
            // suffix.
            let prefix = if nested_name.contains('[') {
                let suffix = format!("[{}]", nested_sf.name);
                nested_name
                    .strip_suffix(&suffix)
                    .unwrap_or(nested_name)
                    .to_string()
            } else {
                String::new()
            };

            let errors = opts.errors.clone();
            build_single_field_context(
                nested_sf,
                &values,
                &errors,
                &prefix,
                opts.non_default_locale,
                opts.depth + 1,
            )
        }
    }
}

/// Enrich a nested Group sub-field context.
///
/// Group inside Array/Blocks uses `[0]` index notation to match the form parser's
/// convention (Group is treated as a single-element composite). For example,
/// `items[0][meta][0][title]` for field "title" inside Group "meta" inside Array "items".
pub(super) fn sub_group(
    gf: &mut GroupField,
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &SubFieldOpts,
) {
    let group_obj = match raw_value {
        Some(Value::Object(_)) => raw_value,
        _ => None,
    };

    let nested_sub_fields: Vec<FieldContext> = sf
        .fields
        .iter()
        .map(|nested_sf| {
            let nested_raw = group_obj
                .and_then(|v| v.as_object())
                .and_then(|m| m.get(&nested_sf.name));

            let nested_name = group_child_name(indexed_name, nested_sf);
            let nested_val = group_child_value(nested_raw, nested_sf);

            build_group_child(
                nested_sf,
                nested_raw,
                &nested_name,
                &nested_val,
                group_obj,
                opts,
            )
        })
        .collect();

    gf.sub_fields = nested_sub_fields;
    gf.collapsed = sf.admin.collapsed;
}

/// Enrich a nested Row sub-field context.
pub(super) fn sub_row_collapsible_row(
    rf: &mut RowField,
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &SubFieldOpts,
) {
    rf.sub_fields = build_enriched_children_from_data(
        &sf.fields,
        raw_value,
        indexed_name,
        opts.locale_locked,
        opts.non_default_locale,
        opts.depth + 1,
        opts.errors,
    );
}

/// Enrich a nested Collapsible sub-field context.
pub(super) fn sub_row_collapsible_group(
    gf: &mut GroupField,
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &SubFieldOpts,
) {
    gf.sub_fields = build_enriched_children_from_data(
        &sf.fields,
        raw_value,
        indexed_name,
        opts.locale_locked,
        opts.non_default_locale,
        opts.depth + 1,
        opts.errors,
    );
    gf.collapsed = sf.admin.collapsed;
}

/// Build a single typed [`TabPanel`] with sub_fields and error count.
fn build_tab_context(
    tab: &crate::core::field::FieldTab,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &SubFieldOpts,
) -> TabPanel {
    let tab_sub_fields = build_enriched_children_from_data(
        &tab.fields,
        raw_value,
        indexed_name,
        opts.locale_locked,
        opts.non_default_locale,
        opts.depth + 1,
        opts.errors,
    );

    let error_count = count_errors_in_field_contexts(&tab_sub_fields);

    TabPanel {
        label: tab.label.clone(),
        sub_fields: tab_sub_fields,
        error_count: if error_count > 0 {
            Some(error_count)
        } else {
            None
        },
        description: tab.description.clone(),
    }
}

/// Enrich a nested Tabs sub-field context.
pub(super) fn sub_tabs(
    tf: &mut TabsField,
    sf: &FieldDefinition,
    raw_value: Option<&Value>,
    indexed_name: &str,
    opts: &SubFieldOpts,
) {
    tf.tabs = sf
        .tabs
        .iter()
        .map(|tab| build_tab_context(tab, raw_value, indexed_name, opts))
        .collect();
}

/// Enrich a Text has_many sub-field context (tag input).
pub(super) fn sub_text_has_many_tags(tf: &mut TextField, val: &str) {
    let tags: Vec<String> = from_str(val).unwrap_or_default();
    tf.base.value = Value::String(tags.join(","));
    tf.has_many = Some(true);
    tf.tags = Some(tags);
}

/// Enrich a Number has_many sub-field context (tag input).
pub(super) fn sub_number_has_many_tags(nf: &mut NumberField, val: &str) {
    let tags: Vec<String> = from_str(val).unwrap_or_default();
    nf.base.value = Value::String(tags.join(","));
    nf.has_many = Some(true);
    nf.tags = Some(tags);
}
