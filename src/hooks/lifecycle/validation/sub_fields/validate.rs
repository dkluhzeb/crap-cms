use std::collections::HashMap;

use mlua::Lua;
use serde_json::{Map as JsonMap, Value};
use tracing::warn;

use crate::{
    core::{FieldDefinition, FieldType, Registry, validate::FieldError},
    hooks::lifecycle::validation::{
        checks,
        checks::is_valid_date_format,
        custom::run_validate_function_inner,
        richtext_attrs::{RichtextValidationCtx, validate_richtext_node_attrs},
    },
};

/// Stable context shared across recursive validation calls for a single row.
struct RowValidationCtx<'a> {
    lua: &'a Lua,
    row_obj: &'a JsonMap<String, Value>,
    row_data: &'a HashMap<String, Value>,
    parent_name: &'a str,
    idx: usize,
    table: &'a str,
    registry: Option<&'a Registry>,
    is_draft: bool,
}

/// Parameters for sub-field validation within a single array/blocks row.
pub(in crate::hooks::lifecycle::validation) struct SubFieldParams<'a> {
    pub lua: &'a Lua,
    pub parent_name: &'a str,
    pub idx: usize,
    pub table: &'a str,
    pub registry: Option<&'a Registry>,
    pub is_draft: bool,
}

/// Validate sub-fields within a single array/blocks row (inner, no mutex).
pub(in crate::hooks::lifecycle::validation) fn validate_sub_fields_inner(
    params: &SubFieldParams<'_>,
    sub_fields: &[FieldDefinition],
    row_obj: &JsonMap<String, Value>,
    errors: &mut Vec<FieldError>,
) {
    let row_data: HashMap<String, Value> = row_obj
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let ctx = RowValidationCtx {
        lua: params.lua,
        row_obj,
        row_data: &row_data,
        parent_name: params.parent_name,
        idx: params.idx,
        table: params.table,
        registry: params.registry,
        is_draft: params.is_draft,
    };

    validate_children_recursive(&ctx, sub_fields, "", errors);
}

/// Recursively validate fields within an array/blocks row, handling arbitrary
/// nesting of layout containers (Group, Row, Collapsible, Tabs).
///
/// `group_prefix` accumulates the `__`-separated group path for data key lookups
/// (e.g. `""` at top level, `"meta__"` inside group `meta`).
fn validate_children_recursive(
    ctx: &RowValidationCtx<'_>,
    fields: &[FieldDefinition],
    group_prefix: &str,
    errors: &mut Vec<FieldError>,
) {
    for sf in fields {
        match sf.field_type {
            FieldType::Group => {
                // Navigate into the Group's nested object and validate children.
                // Form parser stores Group data as nested objects (e.g., {"meta": {"title": "..."}}).
                let data_key = format!("{}{}", group_prefix, sf.name);

                if let Some(group_val) = ctx.row_obj.get(&data_key)
                    && let Some(group_obj) = group_val.as_object()
                {
                    let qualified = format!("{}[{}][{}]", ctx.parent_name, ctx.idx, data_key);
                    let params = SubFieldParams {
                        lua: ctx.lua,
                        parent_name: &qualified,
                        idx: 0,
                        table: ctx.table,
                        registry: ctx.registry,
                        is_draft: ctx.is_draft,
                    };

                    validate_sub_fields_inner(&params, &sf.fields, group_obj, errors);
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                validate_children_recursive(ctx, &sf.fields, group_prefix, errors);
            }
            FieldType::Tabs => {
                for tab in &sf.tabs {
                    validate_children_recursive(ctx, &tab.fields, group_prefix, errors);
                }
            }
            FieldType::Array | FieldType::Blocks => {
                let data_key = format!("{}{}", group_prefix, sf.name);
                let qualified = format!("{}[{}][{}]", ctx.parent_name, ctx.idx, data_key);

                validate_leaf_sub_field(ctx, sf, ctx.row_obj.get(&data_key), &qualified, errors);

                if let Some(Value::Array(nested_rows)) = ctx.row_obj.get(&data_key) {
                    validate_nested_rows(ctx, sf, nested_rows, &qualified, errors);
                }
            }
            _ => {
                let data_key = format!("{}{}", group_prefix, sf.name);
                let qualified = format!("{}[{}][{}]", ctx.parent_name, ctx.idx, data_key);

                validate_leaf_sub_field(ctx, sf, ctx.row_obj.get(&data_key), &qualified, errors);
            }
        }
    }
}

/// Recurse into nested array/blocks rows, resolving block type fields and validating each row.
fn validate_nested_rows(
    ctx: &RowValidationCtx<'_>,
    sf: &FieldDefinition,
    nested_rows: &[Value],
    qualified: &str,
    errors: &mut Vec<FieldError>,
) {
    for (nested_idx, nested_row) in nested_rows.iter().enumerate() {
        let Some(nested_obj) = nested_row.as_object() else {
            continue;
        };

        let sub_fields: &[FieldDefinition] = if sf.field_type == FieldType::Blocks {
            let bt = nested_obj
                .get("_block_type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let Some(bd) = sf.blocks.iter().find(|b| b.block_type == bt) else {
                continue;
            };

            &bd.fields
        } else {
            &sf.fields
        };

        let params = SubFieldParams {
            lua: ctx.lua,
            parent_name: qualified,
            idx: nested_idx,
            table: ctx.table,
            registry: ctx.registry,
            is_draft: ctx.is_draft,
        };

        validate_sub_fields_inner(&params, sub_fields, nested_obj, errors);
    }
}

/// Validate a single leaf sub-field inside an array/blocks row container (Group, Row,
/// Collapsible, or Tabs). Runs the required check, date format check, custom Lua
/// validate function, and richtext node attr validation.
fn validate_leaf_sub_field(
    ctx: &RowValidationCtx<'_>,
    sf: &FieldDefinition,
    value: Option<&Value>,
    qualified_name: &str,
    errors: &mut Vec<FieldError>,
) {
    let is_empty = match value {
        None => true,
        Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        _ => false,
    };

    // 1. Required check (skip for Checkbox — absent/false is valid, skip for drafts)
    if sf.required && is_empty && !ctx.is_draft && sf.field_type != FieldType::Checkbox {
        errors.push(FieldError::with_key(
            qualified_name.to_owned(),
            format!("{} is required", sf.name),
            "validation.required",
            HashMap::from([("field".to_string(), sf.name.clone())]),
        ));
    }

    // 2. Date format check
    if sf.field_type == FieldType::Date
        && !is_empty
        && let Some(Value::String(s)) = value
        && !is_valid_date_format(s)
    {
        errors.push(FieldError::with_key(
            qualified_name.to_owned(),
            format!("{} is not a valid date format", sf.name),
            "validation.invalid_date",
            HashMap::from([("field".to_string(), sf.name.clone())]),
        ));
    }

    // 3. Custom Lua validate function
    if let Some(ref validate_ref) = sf.validate
        && let Some(val) = value
    {
        match run_validate_function_inner(
            ctx.lua,
            validate_ref,
            val,
            ctx.row_data,
            ctx.table,
            &sf.name,
        ) {
            Ok(Some(err_msg)) => {
                errors.push(FieldError::new(qualified_name.to_owned(), err_msg));
            }
            Ok(None) => {}
            Err(e) => {
                warn!("Validate function '{}' error: {}", validate_ref, e);

                errors.push(FieldError::with_key(
                    qualified_name.to_owned(),
                    format!("Validation failed (internal error in '{}')", validate_ref),
                    "validation.custom_error",
                    HashMap::from([("field".to_string(), sf.name.clone())]),
                ));
            }
        }
    }

    // 4. Length bounds (min_length / max_length)
    checks::check_length_bounds(sf, qualified_name, value, is_empty, errors);

    // 5. Numeric bounds (min / max)
    checks::check_numeric_bounds(sf, qualified_name, value, is_empty, errors);

    // 6. Email format validation
    checks::check_email_format(sf, qualified_name, value, is_empty, errors);

    // 7. Select/radio option validation
    checks::check_option_valid(sf, qualified_name, value, is_empty, errors);

    // 8. Has-many element validation (per-element length/numeric bounds, row counts)
    checks::check_has_many_elements(sf, qualified_name, value, is_empty, errors);

    // 9. Richtext node attr validation
    if sf.field_type == FieldType::Richtext
        && !is_empty
        && !sf.admin.nodes.is_empty()
        && let Some(registry) = ctx.registry
        && let Some(Value::String(content)) = value
    {
        validate_richtext_node_attrs(
            &RichtextValidationCtx::builder(ctx.lua, registry, ctx.table)
                .draft(ctx.is_draft)
                .build(),
            content,
            qualified_name,
            sf,
            errors,
        );
    }
}
