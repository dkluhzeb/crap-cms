//! Builds enriched typed child field contexts for layout wrappers (Row,
//! Collapsible, Tabs) inside Array and Blocks rows.
//!
//! Called from `sub_row_collapsible_row`, `sub_row_collapsible_group`, and
//! `build_tab_context` in [`field_types`](super::field_types) when the layout
//! wrapper sits inside an Array/Blocks row. The naming convention mirrors
//! enrichment-phase semantics:
//!
//! - Layout wrappers (Row/Collapsible/Tabs) are transparent — their children
//!   inherit the parent's bracketed name (e.g. `items[0][title]`, not
//!   `items[0][row][title]`).
//! - Group children get a `[0]` suffix in their parent prefix
//!   (e.g. `items[0][meta][0][title]`).
//! - Array/Blocks inside layout wrappers render template-only (no row
//!   iteration). Their data isn't recursed; only the new-row template
//!   sub-fields are populated.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    admin::{
        context::field::{
            ArrayField, BaseFieldData, BlockDefinition, BlocksField, ConditionData, FieldContext,
            TabPanel, ValidationAttrs,
        },
        handlers::{
            field_context::{
                MAX_FIELD_DEPTH,
                builder::build_single_field_context,
                count_errors_in_field_contexts,
                enrich::{field_types, nested::construct_sub_variant, nested::enrich_sub_richtext},
                safe_template_id,
            },
            shared::auto_label_from_name,
        },
    },
    core::field::{FieldDefinition, FieldType},
};

/// Inheritance state passed down through recursion in this module.
struct ChildEnrichOpts<'a> {
    locale_locked: bool,
    non_default_locale: bool,
    depth: usize,
    errors: &'a HashMap<String, String>,
}

/// Resolve the child's form name and raw JSON value.
///
/// Layout wrappers are transparent — they inherit the parent name and the full
/// data object. Leaf fields get `parent_name[field_name]` and their own value.
fn resolve_child_name_and_value<'a>(
    child: &FieldDefinition,
    data: Option<&'a Value>,
    data_obj: Option<&'a serde_json::Map<String, Value>>,
    parent_name: &str,
) -> (String, Option<&'a Value>, String) {
    let is_wrapper = matches!(
        child.field_type,
        FieldType::Tabs | FieldType::Row | FieldType::Collapsible
    );

    let child_raw = if is_wrapper {
        data
    } else {
        data_obj.and_then(|m| m.get(&child.name))
    };

    let child_name = if is_wrapper {
        parent_name.to_string()
    } else {
        format!("{}[{}]", parent_name, child.name)
    };

    let child_val = child_raw
        .map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Null => String::new(),
            _ if is_wrapper => String::new(),
            other => other.to_string(),
        })
        .unwrap_or_default();

    (child_name, child_raw, child_val)
}

/// Build the typed shared base data for a child field. `locale_locked` is
/// recomputed per child as `non_default_locale && !child.localized`, matching
/// the build-phase semantics. A localized field inside a non-localized layout
/// wrapper must stay editable in non-default locales.
fn build_child_base(
    child: &FieldDefinition,
    child_name: &str,
    child_val: &str,
    non_default_locale: bool,
    errors: &HashMap<String, String>,
) -> BaseFieldData {
    let label = child
        .admin
        .label
        .as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| auto_label_from_name(&child.name));

    let locale_locked = non_default_locale && !child.localized;

    BaseFieldData {
        name: child_name.to_string(),
        field_name: child.name.clone(),
        label,
        required: child.required,
        value: Value::String(child_val.to_string()),
        placeholder: child
            .admin
            .placeholder
            .as_ref()
            .map(|ls| ls.resolve_default().to_string()),
        description: child
            .admin
            .description
            .as_ref()
            .map(|ls| ls.resolve_default().to_string()),
        readonly: child.admin.readonly || locale_locked,
        localized: child.localized,
        locale_locked,
        position: child.admin.position.clone(),
        template: child.admin.template.clone(),
        extra: child.admin.extra.clone(),
        error: errors.get(child_name).cloned(),
        validation: ValidationAttrs::default(),
        condition: ConditionData::default(),
    }
}

/// Apply Array template-only enrichment (no row iteration). Layout-wrapper
/// children render only the new-row template sub-fields; existing data rows
/// aren't recursed into here.
fn apply_array_template(
    af: &mut ArrayField,
    child: &FieldDefinition,
    child_name: &str,
    opts: &ChildEnrichOpts,
) {
    let template_prefix = format!("{}[__INDEX__]", child_name);

    af.sub_fields = child
        .fields
        .iter()
        .map(|sf| {
            build_single_field_context(
                sf,
                &HashMap::new(),
                &HashMap::new(),
                &template_prefix,
                opts.non_default_locale,
                opts.depth + 1,
            )
        })
        .collect();

    af.row_count = 0;
    af.template_id = safe_template_id(child_name);
    af.min_rows = child.min_rows;
    af.max_rows = child.max_rows;
    af.init_collapsed = child.admin.collapsed;
    af.add_label = child
        .admin
        .labels_singular
        .as_ref()
        .map(|ls| ls.resolve_default().to_string());
    af.label_field = child.admin.label_field.clone();
}

/// Apply Blocks template-only enrichment (no row iteration). Mirrors
/// [`apply_array_template`] — only the new-row block-definition templates
/// are populated.
fn apply_blocks_template(
    bf: &mut BlocksField,
    child: &FieldDefinition,
    child_name: &str,
    opts: &ChildEnrichOpts,
) {
    let template_prefix = format!("{}[__INDEX__]", child_name);

    bf.block_definitions = child
        .blocks
        .iter()
        .map(|bd| {
            let fields: Vec<FieldContext> = bd
                .fields
                .iter()
                .map(|sf| {
                    build_single_field_context(
                        sf,
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

            BlockDefinition {
                block_type: bd.block_type.clone(),
                label,
                fields,
                label_field: bd.label_field.clone(),
                group: bd.group.clone(),
                image_url: bd.image_url.clone(),
            }
        })
        .collect();

    bf.row_count = 0;
    bf.template_id = safe_template_id(child_name);
    bf.min_rows = child.min_rows;
    bf.max_rows = child.max_rows;
    bf.init_collapsed = child.admin.collapsed;
    bf.add_label = child
        .admin
        .labels_singular
        .as_ref()
        .map(|ls| ls.resolve_default().to_string());
    bf.picker = child.admin.picker.clone();
}

/// Apply Date enrichment with structured-row timezone lookup.
///
/// Two-step: call `sub_date` with an empty `tz_value` (so the displayed
/// value is the raw stored value, no UTC→local conversion), then override
/// `timezone_value` from the parent row's `<short_name>_tz` companion key.
/// The displayed value is intentionally NOT reconverted — the build-phase
/// equivalent has the same gap, and unifying that conversion across both
/// phases is out of scope here.
fn apply_date(
    df: &mut crate::admin::context::field::DateField,
    child: &FieldDefinition,
    child_val: &str,
    data_obj: Option<&serde_json::Map<String, Value>>,
) {
    field_types::sub_date(df, child, child_val, "");

    if !child.timezone {
        return;
    }

    let tz_key = format!("{}_tz", child.name);
    let tz_val = data_obj
        .and_then(|m| m.get(&tz_key))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if !tz_val.is_empty() {
        df.timezone_value = Some(tz_val.to_string());
    }
}

/// Apply type-specific enrichment to the typed child variant.
fn dispatch_child(
    fc: &mut FieldContext,
    child: &FieldDefinition,
    child_raw: Option<&Value>,
    data_obj: Option<&serde_json::Map<String, Value>>,
    child_name: &str,
    child_val: &str,
    opts: &ChildEnrichOpts,
) {
    if opts.depth + 1 >= MAX_FIELD_DEPTH {
        return;
    }

    match fc {
        FieldContext::Row(rf) => {
            rf.sub_fields = build_enriched_children_from_data(
                &child.fields,
                child_raw,
                child_name,
                opts.locale_locked,
                opts.non_default_locale,
                opts.depth + 1,
                opts.errors,
            );
        }
        FieldContext::Collapsible(gf) => {
            gf.sub_fields = build_enriched_children_from_data(
                &child.fields,
                child_raw,
                child_name,
                opts.locale_locked,
                opts.non_default_locale,
                opts.depth + 1,
                opts.errors,
            );
            gf.collapsed = child.admin.collapsed;
        }
        FieldContext::Group(gf) => {
            // Group adds a `[0]` index suffix for its sub-fields' parent
            // prefix — matches the form parser's "Group is a single-element
            // composite" convention.
            let group_prefix = format!("{}[0]", child_name);
            gf.sub_fields = build_enriched_children_from_data(
                &child.fields,
                child_raw,
                &group_prefix,
                opts.locale_locked,
                opts.non_default_locale,
                opts.depth + 1,
                opts.errors,
            );
            gf.collapsed = child.admin.collapsed;
        }
        FieldContext::Tabs(tf) => {
            tf.tabs = child
                .tabs
                .iter()
                .map(|tab| {
                    let tab_sub_fields = build_enriched_children_from_data(
                        &tab.fields,
                        child_raw,
                        child_name,
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
                })
                .collect();
        }
        FieldContext::Array(af) => apply_array_template(af, child, child_name, opts),
        FieldContext::Blocks(bf) => apply_blocks_template(bf, child, child_name, opts),
        FieldContext::Checkbox(cf) => field_types::sub_checkbox(cf, child_val),
        FieldContext::Select(cf) | FieldContext::Radio(cf) => {
            field_types::sub_select_radio(cf, child, child_val);
        }
        FieldContext::Date(df) => apply_date(df, child, child_val, data_obj),
        FieldContext::Relationship(rf) => field_types::sub_relationship(rf, child),
        FieldContext::Upload(uf) => field_types::sub_upload(uf, child),
        FieldContext::Textarea(tf) => {
            tf.rows = child.admin.rows.unwrap_or(8);
            tf.resizable = child.admin.resizable;
        }
        FieldContext::Richtext(rf) => enrich_sub_richtext(rf, child, child_name, opts.errors),
        FieldContext::Code(cf) => {
            // Layout-wrapper-nested Code fields use the operator default
            // language only — no per-row `<short>_lang` lookup. This
            // preserves the pre-existing Value-based behavior; fixing the
            // companion lookup is a separate concern.
            cf.language = child
                .admin
                .language
                .as_deref()
                .unwrap_or("json")
                .to_string();
            if !child.admin.languages.is_empty() {
                cf.languages = Some(child.admin.languages.clone());
            }
        }
        FieldContext::Text(tf) if child.has_many => {
            field_types::sub_text_has_many_tags(tf, child_val);
        }
        FieldContext::Number(nf) if child.has_many => {
            field_types::sub_number_has_many_tags(nf, child_val);
        }
        _ => {}
    }
}

/// Build the typed enriched [`FieldContext`] for one child field.
fn build_child(
    child: &FieldDefinition,
    data: Option<&Value>,
    data_obj: Option<&serde_json::Map<String, Value>>,
    parent_name: &str,
    opts: &ChildEnrichOpts,
) -> FieldContext {
    let (child_name, child_raw, child_val) =
        resolve_child_name_and_value(child, data, data_obj, parent_name);

    let base = build_child_base(
        child,
        &child_name,
        &child_val,
        opts.non_default_locale,
        opts.errors,
    );

    let mut fc = construct_sub_variant(child, base, &child_name);

    dispatch_child(
        &mut fc,
        child,
        child_raw,
        data_obj,
        &child_name,
        &child_val,
        opts,
    );

    fc
}

/// Build typed enriched child field contexts from structured JSON data.
///
/// Used by layout wrapper handlers (Tabs/Row/Collapsible) inside Array/Blocks
/// rows to correctly propagate structured data to nested layout wrappers.
pub fn build_enriched_children_from_data(
    fields: &[FieldDefinition],
    data: Option<&Value>,
    parent_name: &str,
    locale_locked: bool,
    non_default_locale: bool,
    depth: usize,
    errors: &HashMap<String, String>,
) -> Vec<FieldContext> {
    if depth >= MAX_FIELD_DEPTH {
        return Vec::new();
    }

    let data_obj = data.and_then(|v| v.as_object());

    let opts = ChildEnrichOpts {
        locale_locked,
        non_default_locale,
        depth,
        errors,
    };

    fields
        .iter()
        .map(|child| build_child(child, data, data_obj, parent_name, &opts))
        .collect()
}
