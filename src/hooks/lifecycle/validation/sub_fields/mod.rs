use std::collections::HashMap;

use mlua::Lua;
use serde_json::{Map as JsonMap, Value};

use crate::core::{
    field::{FieldDefinition, FieldType},
    validate::FieldError,
};

use super::{checks::is_valid_date_format, custom::run_validate_function_inner};

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

    for sf in sub_fields {
        let sf_value = row_obj.get(&sf.name);
        let sf_empty = match sf_value {
            None => true,
            Some(Value::Null) => true,
            Some(Value::String(s)) => s.is_empty(),
            _ => false,
        };
        let qualified_name = format!("{}[{}][{}]", parent_name, idx, sf.name);

        if sf.required && sf_empty && sf.field_type != FieldType::Checkbox {
            errors.push(FieldError::with_key(
                qualified_name.clone(),
                format!("{} is required", sf.name),
                "validation.required",
                HashMap::from([("field".to_string(), sf.name.clone())]),
            ));
        }

        if sf.field_type == FieldType::Date
            && !sf_empty
            && let Some(Value::String(s)) = sf_value
            && !is_valid_date_format(s)
        {
            errors.push(FieldError::with_key(
                qualified_name.clone(),
                format!("{} is not a valid date format", sf.name),
                "validation.invalid_date",
                HashMap::from([("field".to_string(), sf.name.clone())]),
            ));
        }

        if let Some(ref validate_ref) = sf.validate
            && let Some(val) = sf_value
        {
            match run_validate_function_inner(lua, validate_ref, val, &row_data, table, &sf.name) {
                Ok(Some(err_msg)) => {
                    errors.push(FieldError::new(qualified_name.clone(), err_msg));
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!("Validate function '{}' error: {}", validate_ref, e);
                }
            }
        }

        if matches!(sf.field_type, FieldType::Array | FieldType::Blocks)
            && let Some(Value::Array(nested_rows)) = sf_value
        {
            let nested_parent = format!("{}[{}][{}]", parent_name, idx, sf.name);
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
                        lua,
                        nested_sub_fields,
                        nested_obj,
                        &nested_parent,
                        nested_idx,
                        table,
                        errors,
                    );
                }
            }
        }

        if sf.field_type == FieldType::Group {
            for gsf in &sf.fields {
                let group_key = format!("{}__{}", sf.name, gsf.name);
                let g_qualified = format!("{}[{}][{}]", parent_name, idx, group_key);
                validate_leaf_sub_field(
                    lua,
                    gsf,
                    row_obj.get(&group_key),
                    &g_qualified,
                    &row_data,
                    table,
                    errors,
                );
            }
        }

        // Row sub-fields within arrays use plain sub-field names (no prefix)
        if sf.field_type == FieldType::Row {
            for rsf in &sf.fields {
                let r_qualified = format!("{}[{}][{}]", parent_name, idx, rsf.name);
                validate_leaf_sub_field(
                    lua,
                    rsf,
                    row_obj.get(&rsf.name),
                    &r_qualified,
                    &row_data,
                    table,
                    errors,
                );
            }
        }

        // Collapsible sub-fields within arrays (same as Row)
        if sf.field_type == FieldType::Collapsible {
            for csf in &sf.fields {
                let c_qualified = format!("{}[{}][{}]", parent_name, idx, csf.name);
                validate_leaf_sub_field(
                    lua,
                    csf,
                    row_obj.get(&csf.name),
                    &c_qualified,
                    &row_data,
                    table,
                    errors,
                );
            }
        }

        // Tabs sub-fields within arrays (iterate tab.fields)
        if sf.field_type == FieldType::Tabs {
            for tab in &sf.tabs {
                for tsf in &tab.fields {
                    let t_qualified = format!("{}[{}][{}]", parent_name, idx, tsf.name);
                    validate_leaf_sub_field(
                        lua,
                        tsf,
                        row_obj.get(&tsf.name),
                        &t_qualified,
                        &row_data,
                        table,
                        errors,
                    );
                }
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
