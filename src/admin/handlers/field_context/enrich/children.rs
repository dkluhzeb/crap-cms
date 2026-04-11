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

/// Parameters for building enriched child contexts.
struct EnrichChildrenParams<'a> {
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

/// Build the base JSON context for a child field (before type-specific enrichment).
fn build_child_base_context(
    child: &FieldDefinition,
    child_name: &str,
    child_val: &str,
    locale_locked: bool,
    errors: &HashMap<String, String>,
) -> Value {
    let child_label = child
        .admin
        .label
        .as_ref()
        .map(|ls| ls.resolve_default().to_string())
        .unwrap_or_else(|| auto_label_from_name(&child.name));

    let mut ctx = json!({
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

    if let Some(err) = errors.get(child_name) {
        ctx["error"] = json!(err);
    }

    ctx
}

/// Enrich a Row or Collapsible wrapper with recursive sub_fields.
fn enrich_wrapper(
    child: &FieldDefinition,
    child_raw: Option<&Value>,
    child_name: &str,
    ctx: &mut Value,
    params: &EnrichChildrenParams,
) {
    let sub = recurse(params, &child.fields, child_raw, child_name);
    ctx["sub_fields"] = json!(sub);

    if child.field_type == FieldType::Collapsible {
        ctx["collapsed"] = json!(child.admin.collapsed);
    }
}

/// Enrich a Group field with `[0]`-prefixed sub_fields for form parser compatibility.
fn enrich_group(
    child: &FieldDefinition,
    child_raw: Option<&Value>,
    child_name: &str,
    ctx: &mut Value,
    params: &EnrichChildrenParams,
) {
    let group_prefix = format!("{}[0]", child_name);
    let sub = recurse(params, &child.fields, child_raw, &group_prefix);
    ctx["sub_fields"] = json!(sub);
    ctx["collapsed"] = json!(child.admin.collapsed);
}

/// Enrich a leaf field (Text, Checkbox, Select, Date, etc.) with type-specific extras.
fn enrich_leaf(
    child: &FieldDefinition,
    child_name: &str,
    child_val: &str,
    data_obj: Option<&serde_json::Map<String, Value>>,
    ctx: &mut Value,
    params: &EnrichChildrenParams,
) {
    let empty = HashMap::new();
    let extras_ctx = FieldRecursionCtx::builder(&empty, params.errors, child_name)
        .non_default_locale(params.non_default_locale)
        .depth(params.depth + 1)
        .build();

    apply_field_type_extras(child, child_val, ctx, &extras_ctx);

    if child.field_type == FieldType::Date && child.timezone {
        inject_timezone(child, data_obj, ctx);
    }
}

/// Apply type-specific enrichment to a child context (recursive sub_fields, tabs, extras).
fn enrich_by_field_type(
    child: &FieldDefinition,
    child_raw: Option<&Value>,
    child_name: &str,
    child_val: &str,
    data_obj: Option<&serde_json::Map<String, Value>>,
    ctx: &mut Value,
    params: &EnrichChildrenParams,
) {
    match child.field_type {
        FieldType::Row | FieldType::Collapsible => {
            enrich_wrapper(child, child_raw, child_name, ctx, params);
        }
        FieldType::Group => {
            enrich_group(child, child_raw, child_name, ctx, params);
        }
        FieldType::Tabs => {
            ctx["tabs"] = json!(build_tabs_context(child, child_raw, child_name, params));
        }
        _ => {
            enrich_leaf(child, child_name, child_val, data_obj, ctx, params);
        }
    }
}

/// Recursively build enriched children (delegates back to the public entry point).
fn recurse(
    params: &EnrichChildrenParams,
    fields: &[FieldDefinition],
    data: Option<&Value>,
    parent_name: &str,
) -> Vec<Value> {
    build_enriched_children_from_data(
        fields,
        data,
        parent_name,
        params.locale_locked,
        params.non_default_locale,
        params.depth + 1,
        params.errors,
    )
}

/// Build tab contexts with sub_fields and error counts.
fn build_tabs_context(
    child: &FieldDefinition,
    child_raw: Option<&Value>,
    child_name: &str,
    params: &EnrichChildrenParams,
) -> Vec<Value> {
    child
        .tabs
        .iter()
        .map(|tab| {
            let tab_sub_fields = recurse(params, &tab.fields, child_raw, child_name);
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
        .collect()
}

/// Inject stored timezone value from the parent row into a Date field context.
fn inject_timezone(
    child: &FieldDefinition,
    data_obj: Option<&serde_json::Map<String, Value>>,
    ctx: &mut Value,
) {
    let tz_key = format!("{}_tz", child.name);
    let tz_val = data_obj
        .and_then(|m| m.get(&tz_key))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if !tz_val.is_empty() {
        ctx["timezone_value"] = json!(tz_val);
    }
}

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

    let params = EnrichChildrenParams {
        locale_locked,
        non_default_locale,
        depth,
        errors,
    };

    fields
        .iter()
        .map(|child| {
            let (child_name, child_raw, child_val) =
                resolve_child_name_and_value(child, data, data_obj, parent_name);

            let mut ctx =
                build_child_base_context(child, &child_name, &child_val, locale_locked, errors);

            enrich_by_field_type(
                child,
                child_raw,
                &child_name,
                &child_val,
                data_obj,
                &mut ctx,
                &params,
            );

            ctx
        })
        .collect()
}
