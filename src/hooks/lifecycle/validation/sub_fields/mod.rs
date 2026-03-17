use std::collections::HashMap;

use mlua::Lua;
use serde_json::{Map as JsonMap, Value};

use crate::core::{FieldDefinition, FieldType, validate::FieldError};

use super::{checks::is_valid_date_format, custom::run_validate_function_inner};

/// Stable context shared across recursive validation calls for a single row.
struct RowValidationCtx<'a> {
    lua: &'a Lua,
    row_obj: &'a JsonMap<String, Value>,
    row_data: &'a HashMap<String, Value>,
    parent_name: &'a str,
    idx: usize,
    table: &'a str,
}

/// Validate sub-fields within a single array/blocks row (inner, no mutex).
pub(super) fn validate_sub_fields_inner(
    lua: &Lua,
    sub_fields: &[FieldDefinition],
    row_obj: &JsonMap<String, Value>,
    parent_name: &str,
    idx: usize,
    table: &str,
    errors: &mut Vec<FieldError>,
) {
    let row_data: HashMap<String, Value> = row_obj
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let ctx = RowValidationCtx {
        lua,
        row_obj,
        row_data: &row_data,
        parent_name,
        idx,
        table,
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
                let new_prefix = format!("{}{}__", group_prefix, sf.name);
                validate_children_recursive(ctx, &sf.fields, &new_prefix, errors);
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
                // Validate the array/blocks field itself (required, custom validate)
                validate_leaf_sub_field(
                    ctx.lua,
                    sf,
                    ctx.row_obj.get(&data_key),
                    &qualified,
                    ctx.row_data,
                    ctx.table,
                    errors,
                );
                // Recurse into nested rows
                if let Some(Value::Array(nested_rows)) = ctx.row_obj.get(&data_key) {
                    for (nested_idx, nested_row) in nested_rows.iter().enumerate() {
                        if let Some(nested_obj) = nested_row.as_object() {
                            let nested_sub_fields: &[FieldDefinition] =
                                if sf.field_type == FieldType::Blocks {
                                    let bt = nested_obj
                                        .get("_block_type")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    match sf.blocks.iter().find(|b| b.block_type == bt) {
                                        Some(bd) => &bd.fields,
                                        None => continue,
                                    }
                                } else {
                                    &sf.fields
                                };
                            validate_sub_fields_inner(
                                ctx.lua,
                                nested_sub_fields,
                                nested_obj,
                                &qualified,
                                nested_idx,
                                ctx.table,
                                errors,
                            );
                        }
                    }
                }
            }
            _ => {
                let data_key = format!("{}{}", group_prefix, sf.name);
                let qualified = format!("{}[{}][{}]", ctx.parent_name, ctx.idx, data_key);
                validate_leaf_sub_field(
                    ctx.lua,
                    sf,
                    ctx.row_obj.get(&data_key),
                    &qualified,
                    ctx.row_data,
                    ctx.table,
                    errors,
                );
            }
        }
    }
}

/// Validate a single leaf sub-field inside an array/blocks row container (Group, Row,
/// Collapsible, or Tabs). Runs the required check, date format check, and custom Lua
/// validate function — the same three-step sequence shared by all four container types.
fn validate_leaf_sub_field(
    lua: &Lua,
    sf: &FieldDefinition,
    value: Option<&Value>,
    qualified_name: &str,
    row_data: &HashMap<String, Value>,
    table: &str,
    errors: &mut Vec<FieldError>,
) {
    let is_empty = match value {
        None => true,
        Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        _ => false,
    };

    // 1. Required check (skip for Checkbox — absent/false is valid)
    if sf.required && is_empty && sf.field_type != FieldType::Checkbox {
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
        match run_validate_function_inner(lua, validate_ref, val, row_data, table, &sf.name) {
            Ok(Some(err_msg)) => {
                errors.push(FieldError::new(qualified_name.to_owned(), err_msg));
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("Validate function '{}' error: {}", validate_ref, e);
            }
        }
    }
}

#[cfg(test)]
mod tests;
