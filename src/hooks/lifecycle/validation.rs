//! Field validation logic: required checks, unique checks, date format, custom Lua validators,
//! and display condition evaluation.

use anyhow::Result;
use mlua::{Lua, Value};
use std::collections::HashMap;

use crate::core::field::{FieldDefinition, FieldType};
use crate::core::validate::{FieldError, ValidationError};
use crate::db::query;

use super::resolve_hook_function;

/// Inner implementation of `validate_fields` — operates on a locked `&Lua`.
/// Used by both `HookRunner::validate_fields` and Lua CRUD closures.
pub(super) fn validate_fields_inner(
    lua: &Lua,
    fields: &[FieldDefinition],
    data: &HashMap<String, serde_json::Value>,
    conn: &rusqlite::Connection,
    table: &str,
    exclude_id: Option<&str>,
    is_draft: bool,
) -> Result<(), ValidationError> {
    let mut errors = Vec::new();
    validate_fields_recursive(lua, fields, data, conn, table, exclude_id, is_draft, "", &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationError { errors })
    }
}

/// Recursive validation with prefix support for arbitrary nesting.
/// Group accumulates prefix (`group__`), Row/Collapsible/Tabs pass through.
fn validate_fields_recursive(
    lua: &Lua,
    fields: &[FieldDefinition],
    data: &HashMap<String, serde_json::Value>,
    conn: &rusqlite::Connection,
    table: &str,
    exclude_id: Option<&str>,
    is_draft: bool,
    prefix: &str,
    errors: &mut Vec<FieldError>,
) {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = if prefix.is_empty() {
                    field.name.clone()
                } else {
                    format!("{}__{}", prefix, field.name)
                };
                validate_fields_recursive(
                    lua, &field.fields, data, conn, table, exclude_id, is_draft, &new_prefix, errors,
                );
            }
            FieldType::Row | FieldType::Collapsible => {
                validate_fields_recursive(
                    lua, &field.fields, data, conn, table, exclude_id, is_draft, prefix, errors,
                );
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    validate_fields_recursive(
                        lua, &tab.fields, data, conn, table, exclude_id, is_draft, prefix, errors,
                    );
                }
            }
            FieldType::Join => {
                // Virtual field — no data to validate
            }
            _ => {
                validate_scalar_field(lua, field, data, conn, table, exclude_id, is_draft, prefix, errors);
            }
        }
    }
}

/// Validate a single scalar field (not Group/Row/Collapsible/Tabs).
fn validate_scalar_field(
    lua: &Lua,
    field: &FieldDefinition,
    data: &HashMap<String, serde_json::Value>,
    conn: &rusqlite::Connection,
    table: &str,
    exclude_id: Option<&str>,
    is_draft: bool,
    prefix: &str,
    errors: &mut Vec<FieldError>,
) {
    let data_key = if prefix.is_empty() {
        field.name.clone()
    } else {
        format!("{}__{}", prefix, field.name)
    };

    let value = data.get(&data_key);
    let is_empty = match value {
        None => true,
        Some(serde_json::Value::Null) => true,
        Some(serde_json::Value::String(s)) => s.is_empty(),
        _ => false,
    };

    // Required check (skip for checkboxes — absent = false is valid)
    // For Array and has-many Relationship, "required" means at least one item
    // Skip required checks entirely for draft saves
    // On update (exclude_id set): skip if field not in data (partial update, keep existing)
    let is_update = exclude_id.is_some();
    if field.required && !is_draft && field.field_type != FieldType::Checkbox
        && !(is_update && value.is_none())
    {
        if !field.has_parent_column() {
            let has_items = match value {
                Some(serde_json::Value::Array(arr)) => !arr.is_empty(),
                Some(serde_json::Value::String(s)) => !s.is_empty(),
                _ => false,
            };
            if !has_items {
                errors.push(FieldError {
                    field: data_key.clone(),
                    message: format!("{} is required", field.name),
                });
            }
        } else if field.has_many {
            // has_many text/number/select stored as JSON array string — "[]" = empty
            let has_items = match value {
                Some(serde_json::Value::String(s)) => {
                    serde_json::from_str::<Vec<serde_json::Value>>(s)
                        .map(|arr| !arr.is_empty())
                        .unwrap_or(!s.is_empty())
                }
                _ => false,
            };
            if !has_items {
                errors.push(FieldError {
                    field: data_key.clone(),
                    message: format!("{} is required", field.name),
                });
            }
        } else if is_empty {
            errors.push(FieldError {
                field: data_key.clone(),
                message: format!("{} is required", field.name),
            });
        }
    }

    // min_rows / max_rows validation for Array, Blocks, and has-many Relationship
    if !is_draft && (field.min_rows.is_some() || field.max_rows.is_some()) {
        let row_count = match value {
            Some(serde_json::Value::Array(arr)) => arr.len(),
            _ => 0,
        };
        if let Some(min) = field.min_rows {
            if row_count < min {
                errors.push(FieldError {
                    field: data_key.clone(),
                    message: format!("{} requires at least {} item(s)", field.name, min),
                });
            }
        }
        if let Some(max) = field.max_rows {
            if row_count > max {
                errors.push(FieldError {
                    field: data_key.clone(),
                    message: format!("{} allows at most {} item(s)", field.name, max),
                });
            }
        }
    }

    // Validate sub-fields within Array/Blocks rows
    if !is_draft && matches!(field.field_type, FieldType::Array | FieldType::Blocks) {
        if let Some(serde_json::Value::Array(rows)) = value {
            for (idx, row) in rows.iter().enumerate() {
                let row_obj = match row.as_object() {
                    Some(obj) => obj,
                    None => continue,
                };

                let sub_fields: &[FieldDefinition] = if field.field_type == FieldType::Blocks {
                    let block_type = row_obj.get("_block_type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    match field.blocks.iter().find(|b| b.block_type == block_type) {
                        Some(bd) => &bd.fields,
                        None => continue,
                    }
                } else {
                    &field.fields
                };

                validate_sub_fields_inner(
                    lua, sub_fields, row_obj, &data_key, idx, table, errors,
                );
            }
        }
    }

    // Unique check (only if value is non-empty, skip for join-table fields)
    if field.unique && !is_empty && field.has_parent_column() {
        let value_str = match value {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        };
        match query::count_where_field_eq(conn, table, &data_key, &value_str, exclude_id) {
            Ok(count) if count > 0 => {
                errors.push(FieldError {
                    field: data_key.clone(),
                    message: format!("{} must be unique", field.name),
                });
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("Unique check failed for {}.{}: {}", table, data_key, e);
            }
        }
    }

    // min_length / max_length validation (text, textarea — check string length)
    // Skip for has_many fields — their values are JSON arrays, validated per-element below
    if !is_empty && !field.has_many && (field.min_length.is_some() || field.max_length.is_some()) {
        if let Some(serde_json::Value::String(s)) = value {
            let len = s.len();
            if let Some(min_len) = field.min_length {
                if len < min_len {
                    errors.push(FieldError {
                        field: data_key.clone(),
                        message: format!("{} must be at least {} characters", field.name, min_len),
                    });
                }
            }
            if let Some(max_len) = field.max_length {
                if len > max_len {
                    errors.push(FieldError {
                        field: data_key.clone(),
                        message: format!("{} must be at most {} characters", field.name, max_len),
                    });
                }
            }
        }
    }

    // min / max validation (number fields — parse as f64, check bounds)
    // Skip for has_many fields — validated per-element below
    if !is_empty && !field.has_many && (field.min.is_some() || field.max.is_some()) {
        let num_val = match value {
            Some(serde_json::Value::Number(n)) => n.as_f64(),
            Some(serde_json::Value::String(s)) => s.parse::<f64>().ok(),
            _ => None,
        };
        if let Some(v) = num_val {
            if let Some(min_val) = field.min {
                if v < min_val {
                    errors.push(FieldError {
                        field: data_key.clone(),
                        message: format!("{} must be at least {}", field.name, min_val),
                    });
                }
            }
            if let Some(max_val) = field.max {
                if v > max_val {
                    errors.push(FieldError {
                        field: data_key.clone(),
                        message: format!("{} must be at most {}", field.name, max_val),
                    });
                }
            }
        }
    }

    // Email format validation (only if non-empty)
    if field.field_type == FieldType::Email && !is_empty {
        if let Some(serde_json::Value::String(s)) = value {
            if !is_valid_email_format(s) {
                errors.push(FieldError {
                    field: data_key.clone(),
                    message: format!("{} is not a valid email address", field.name),
                });
            }
        }
    }

    // Select/Radio option validation (only if non-empty, check value exists in options)
    if (field.field_type == FieldType::Select || field.field_type == FieldType::Radio)
        && !is_empty && !field.options.is_empty()
    {
        if let Some(serde_json::Value::String(s)) = value {
            if field.has_many {
                // has_many select: value is a JSON array string like '["val1","val2"]'
                if let Ok(values) = serde_json::from_str::<Vec<String>>(s) {
                    for v in &values {
                        if !field.options.iter().any(|opt| opt.value == *v) {
                            errors.push(FieldError {
                                field: data_key.clone(),
                                message: format!("{} has an invalid option: {}", field.name, v),
                            });
                            break;
                        }
                    }
                }
            } else if !field.options.iter().any(|opt| opt.value == *s) {
                errors.push(FieldError {
                    field: data_key.clone(),
                    message: format!("{} has an invalid option", field.name),
                });
            }
        }
    }

    // has_many text/number: validate individual values within the JSON array
    if (field.field_type == FieldType::Text || field.field_type == FieldType::Number)
        && field.has_many && !is_empty
    {
        if let Some(serde_json::Value::String(s)) = value {
            if let Ok(values) = serde_json::from_str::<Vec<String>>(s) {
                // Validate count with min_rows/max_rows
                if let Some(min_rows) = field.min_rows {
                    if values.len() < min_rows {
                        errors.push(FieldError {
                            field: data_key.clone(),
                            message: format!("{} must have at least {} values", field.name, min_rows),
                        });
                    }
                }
                if let Some(max_rows) = field.max_rows {
                    if values.len() > max_rows {
                        errors.push(FieldError {
                            field: data_key.clone(),
                            message: format!("{} must have at most {} values", field.name, max_rows),
                        });
                    }
                }
                // Validate each value
                for v in &values {
                    if field.field_type == FieldType::Text {
                        if let Some(min_len) = field.min_length {
                            if v.len() < min_len {
                                errors.push(FieldError {
                                    field: data_key.clone(),
                                    message: format!("{}: '{}' must be at least {} characters", field.name, v, min_len),
                                });
                                break;
                            }
                        }
                        if let Some(max_len) = field.max_length {
                            if v.len() > max_len {
                                errors.push(FieldError {
                                    field: data_key.clone(),
                                    message: format!("{}: '{}' must be at most {} characters", field.name, v, max_len),
                                });
                                break;
                            }
                        }
                    } else if field.field_type == FieldType::Number {
                        if let Ok(num) = v.parse::<f64>() {
                            if let Some(min_val) = field.min {
                                if num < min_val {
                                    errors.push(FieldError {
                                        field: data_key.clone(),
                                        message: format!("{}: {} must be at least {}", field.name, v, min_val),
                                    });
                                    break;
                                }
                            }
                            if let Some(max_val) = field.max {
                                if num > max_val {
                                    errors.push(FieldError {
                                        field: data_key.clone(),
                                        message: format!("{}: {} must be at most {}", field.name, v, max_val),
                                    });
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Date format validation (only if non-empty)
    if field.field_type == FieldType::Date && !is_empty {
        if let Some(serde_json::Value::String(s)) = value {
            if !is_valid_date_format(s) {
                errors.push(FieldError {
                    field: data_key.clone(),
                    message: format!("{} is not a valid date format", field.name),
                });
            }
            // Date bounds validation (ISO dates sort lexicographically)
            if let Some(ref min_date) = field.min_date {
                // Compare the date portion only (first 10 chars for YYYY-MM-DD)
                let date_part = if s.len() >= 10 { &s[..10] } else { s.as_str() };
                if date_part < min_date.as_str() {
                    errors.push(FieldError {
                        field: data_key.clone(),
                        message: format!("{} must be on or after {}", field.name, min_date),
                    });
                }
            }
            if let Some(ref max_date) = field.max_date {
                let date_part = if s.len() >= 10 { &s[..10] } else { s.as_str() };
                if date_part > max_date.as_str() {
                    errors.push(FieldError {
                        field: data_key.clone(),
                        message: format!("{} must be on or before {}", field.name, max_date),
                    });
                }
            }
        }
    }

    // Custom validate function (Lua)
    if let Some(ref validate_ref) = field.validate {
        if let Some(val) = value {
            match run_validate_function_inner(lua, validate_ref, val, data, table, &field.name) {
                Ok(Some(err_msg)) => {
                    errors.push(FieldError {
                        field: data_key,
                        message: err_msg,
                    });
                }
                Ok(None) => {} // valid
                Err(e) => {
                    tracing::warn!("Validate function '{}' error: {}", validate_ref, e);
                }
            }
        }
    }
}

/// Validate sub-fields within a single array/blocks row (inner, no mutex).
fn validate_sub_fields_inner(
    lua: &Lua,
    sub_fields: &[FieldDefinition],
    row_obj: &serde_json::Map<String, serde_json::Value>,
    parent_name: &str,
    idx: usize,
    table: &str,
    errors: &mut Vec<FieldError>,
) {
    let row_data: HashMap<String, serde_json::Value> = row_obj.iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    for sf in sub_fields {
        let sf_value = row_obj.get(&sf.name);
        let sf_empty = match sf_value {
            None => true,
            Some(serde_json::Value::Null) => true,
            Some(serde_json::Value::String(s)) => s.is_empty(),
            _ => false,
        };
        let qualified_name = format!("{}[{}][{}]", parent_name, idx, sf.name);

        if sf.required && sf_empty && sf.field_type != FieldType::Checkbox {
            errors.push(FieldError {
                field: qualified_name.clone(),
                message: format!("{} is required", sf.name),
            });
        }

        if sf.field_type == FieldType::Date && !sf_empty {
            if let Some(serde_json::Value::String(s)) = sf_value {
                if !is_valid_date_format(s) {
                    errors.push(FieldError {
                        field: qualified_name.clone(),
                        message: format!("{} is not a valid date format", sf.name),
                    });
                }
            }
        }

        if let Some(ref validate_ref) = sf.validate {
            if let Some(val) = sf_value {
                match run_validate_function_inner(lua, validate_ref, val, &row_data, table, &sf.name) {
                    Ok(Some(err_msg)) => {
                        errors.push(FieldError {
                            field: qualified_name.clone(),
                            message: err_msg,
                        });
                    }
                    Ok(None) => {}
                    Err(e) => {
                        tracing::warn!("Validate function '{}' error: {}", validate_ref, e);
                    }
                }
            }
        }

        if matches!(sf.field_type, FieldType::Array | FieldType::Blocks) {
            if let Some(serde_json::Value::Array(nested_rows)) = sf_value {
                let nested_parent = format!("{}[{}][{}]", parent_name, idx, sf.name);
                for (nested_idx, nested_row) in nested_rows.iter().enumerate() {
                    if let Some(nested_obj) = nested_row.as_object() {
                        let nested_sub_fields: &[FieldDefinition] = if sf.field_type == FieldType::Blocks {
                            let bt = nested_obj.get("_block_type")
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
                            lua, nested_sub_fields, nested_obj, &nested_parent, nested_idx, table, errors,
                        );
                    }
                }
            }
        }

        if sf.field_type == FieldType::Group {
            for gsf in &sf.fields {
                let group_key = format!("{}__{}", sf.name, gsf.name);
                let gv = row_obj.get(&group_key);
                let g_empty = match gv {
                    None => true,
                    Some(serde_json::Value::Null) => true,
                    Some(serde_json::Value::String(s)) => s.is_empty(),
                    _ => false,
                };
                let g_qualified = format!("{}[{}][{}]", parent_name, idx, group_key);

                if gsf.required && g_empty && gsf.field_type != FieldType::Checkbox {
                    errors.push(FieldError {
                        field: g_qualified.clone(),
                        message: format!("{} is required", gsf.name),
                    });
                }

                if gsf.field_type == FieldType::Date && !g_empty {
                    if let Some(serde_json::Value::String(s)) = gv {
                        if !is_valid_date_format(s) {
                            errors.push(FieldError {
                                field: g_qualified.clone(),
                                message: format!("{} is not a valid date format", gsf.name),
                            });
                        }
                    }
                }

                if let Some(ref validate_ref) = gsf.validate {
                    if let Some(val) = gv {
                        match run_validate_function_inner(lua, validate_ref, val, &row_data, table, &gsf.name) {
                            Ok(Some(err_msg)) => {
                                errors.push(FieldError {
                                    field: g_qualified,
                                    message: err_msg,
                                });
                            }
                            Ok(None) => {}
                            Err(e) => {
                                tracing::warn!("Validate function '{}' error: {}", validate_ref, e);
                            }
                        }
                    }
                }
            }
        }

        // Row sub-fields within arrays use plain sub-field names (no prefix)
        if sf.field_type == FieldType::Row {
            for rsf in &sf.fields {
                let rv = row_obj.get(&rsf.name);
                let r_empty = match rv {
                    None => true,
                    Some(serde_json::Value::Null) => true,
                    Some(serde_json::Value::String(s)) => s.is_empty(),
                    _ => false,
                };
                let r_qualified = format!("{}[{}][{}]", parent_name, idx, rsf.name);

                if rsf.required && r_empty && rsf.field_type != FieldType::Checkbox {
                    errors.push(FieldError {
                        field: r_qualified.clone(),
                        message: format!("{} is required", rsf.name),
                    });
                }

                if rsf.field_type == FieldType::Date && !r_empty {
                    if let Some(serde_json::Value::String(s)) = rv {
                        if !is_valid_date_format(s) {
                            errors.push(FieldError {
                                field: r_qualified.clone(),
                                message: format!("{} is not a valid date format", rsf.name),
                            });
                        }
                    }
                }

                if let Some(ref validate_ref) = rsf.validate {
                    if let Some(val) = rv {
                        match run_validate_function_inner(lua, validate_ref, val, &row_data, table, &rsf.name) {
                            Ok(Some(err_msg)) => {
                                errors.push(FieldError {
                                    field: r_qualified,
                                    message: err_msg,
                                });
                            }
                            Ok(None) => {}
                            Err(e) => {
                                tracing::warn!("Validate function '{}' error: {}", validate_ref, e);
                            }
                        }
                    }
                }
            }
        }

        // Collapsible sub-fields within arrays (same as Row)
        if sf.field_type == FieldType::Collapsible {
            for csf in &sf.fields {
                let cv = row_obj.get(&csf.name);
                let c_empty = match cv {
                    None => true,
                    Some(serde_json::Value::Null) => true,
                    Some(serde_json::Value::String(s)) => s.is_empty(),
                    _ => false,
                };
                let c_qualified = format!("{}[{}][{}]", parent_name, idx, csf.name);

                if csf.required && c_empty && csf.field_type != FieldType::Checkbox {
                    errors.push(FieldError {
                        field: c_qualified.clone(),
                        message: format!("{} is required", csf.name),
                    });
                }

                if csf.field_type == FieldType::Date && !c_empty {
                    if let Some(serde_json::Value::String(s)) = cv {
                        if !is_valid_date_format(s) {
                            errors.push(FieldError {
                                field: c_qualified.clone(),
                                message: format!("{} is not a valid date format", csf.name),
                            });
                        }
                    }
                }

                if let Some(ref validate_ref) = csf.validate {
                    if let Some(val) = cv {
                        match run_validate_function_inner(lua, validate_ref, val, &row_data, table, &csf.name) {
                            Ok(Some(err_msg)) => {
                                errors.push(FieldError {
                                    field: c_qualified,
                                    message: err_msg,
                                });
                            }
                            Ok(None) => {}
                            Err(e) => {
                                tracing::warn!("Validate function '{}' error: {}", validate_ref, e);
                            }
                        }
                    }
                }
            }
        }

        // Tabs sub-fields within arrays (iterate tab.fields)
        if sf.field_type == FieldType::Tabs {
            for tab in &sf.tabs {
                for tsf in &tab.fields {
                    let tv = row_obj.get(&tsf.name);
                    let t_empty = match tv {
                        None => true,
                        Some(serde_json::Value::Null) => true,
                        Some(serde_json::Value::String(s)) => s.is_empty(),
                        _ => false,
                    };
                    let t_qualified = format!("{}[{}][{}]", parent_name, idx, tsf.name);

                    if tsf.required && t_empty && tsf.field_type != FieldType::Checkbox {
                        errors.push(FieldError {
                            field: t_qualified.clone(),
                            message: format!("{} is required", tsf.name),
                        });
                    }

                    if tsf.field_type == FieldType::Date && !t_empty {
                        if let Some(serde_json::Value::String(s)) = tv {
                            if !is_valid_date_format(s) {
                                errors.push(FieldError {
                                    field: t_qualified.clone(),
                                    message: format!("{} is not a valid date format", tsf.name),
                                });
                            }
                        }
                    }

                    if let Some(ref validate_ref) = tsf.validate {
                        if let Some(val) = tv {
                            match run_validate_function_inner(lua, validate_ref, val, &row_data, table, &tsf.name) {
                                Ok(Some(err_msg)) => {
                                    errors.push(FieldError {
                                        field: t_qualified,
                                        message: err_msg,
                                    });
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    tracing::warn!("Validate function '{}' error: {}", validate_ref, e);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Inner implementation of `run_validate_function` — operates on a locked `&Lua`.
pub(super) fn run_validate_function_inner(
    lua: &Lua,
    func_ref: &str,
    value: &serde_json::Value,
    data: &HashMap<String, serde_json::Value>,
    collection: &str,
    field_name: &str,
) -> Result<Option<String>> {
    let func = resolve_hook_function(lua, func_ref)?;
    let lua_value = crate::hooks::api::json_to_lua(lua, value)?;
    let ctx_table = lua.create_table()?;
    ctx_table.set("collection", collection)?;
    ctx_table.set("field_name", field_name)?;
    let data_table = lua.create_table()?;
    for (k, v) in data {
        data_table.set(k.as_str(), crate::hooks::api::json_to_lua(lua, v)?)?;
    }
    ctx_table.set("data", data_table)?;

    let result: Value = func.call((lua_value, ctx_table))?;
    match result {
        Value::Nil => Ok(None),
        Value::Boolean(true) => Ok(None),
        Value::Boolean(false) => Ok(Some("validation failed".to_string())),
        Value::String(s) => Ok(Some(s.to_str()?.to_string())),
        _ => Ok(None),
    }
}

/// Check if a string is a recognized date format for the date field type.
/// Accepts: YYYY-MM-DD, YYYY-MM-DDTHH:MM, YYYY-MM-DDTHH:MM:SS, full ISO 8601/RFC 3339,
/// HH:MM (time only), HH:MM:SS, YYYY-MM (month only).
fn is_valid_date_format(value: &str) -> bool {
    use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime};

    // Time only: HH:MM or HH:MM:SS
    if value.len() <= 8 && value.contains(':') && !value.contains('T') {
        let parts: Vec<&str> = value.split(':').collect();
        if parts.len() >= 2 {
            return parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()));
        }
    }

    // Month only: YYYY-MM
    if value.len() == 7 && value.as_bytes().get(4) == Some(&b'-') && !value.contains('T') {
        let parts: Vec<&str> = value.split('-').collect();
        if parts.len() == 2 {
            return parts[0].len() == 4
                && parts[0].chars().all(|c| c.is_ascii_digit())
                && parts[1].len() == 2
                && parts[1].chars().all(|c| c.is_ascii_digit());
        }
    }

    // Full RFC 3339
    if DateTime::<FixedOffset>::parse_from_rfc3339(value).is_ok() {
        return true;
    }

    // Date only: YYYY-MM-DD
    if value.len() == 10 {
        return NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok();
    }

    // datetime-local: YYYY-MM-DDTHH:MM
    if value.len() == 16 && value.contains('T') {
        return NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M").is_ok();
    }

    // YYYY-MM-DDTHH:MM:SS (no timezone)
    if value.len() == 19 && value.contains('T') {
        return NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").is_ok();
    }

    false
}

/// Check if a string looks like a valid email address.
/// Simple check: has exactly one @, non-empty local and domain parts, domain has a dot.
fn is_valid_email_format(value: &str) -> bool {
    let parts: Vec<&str> = value.splitn(2, '@').collect();
    if parts.len() != 2 {
        return false;
    }
    let local = parts[0];
    let domain = parts[1];
    !local.is_empty()
        && !domain.is_empty()
        && domain.contains('.')
        && !local.contains(char::is_whitespace)
        && !domain.contains(char::is_whitespace)
}

/// Evaluate a condition table (JSON) against form data.
/// A single condition object has `{ field, equals|not_equals|in|not_in|is_truthy|is_falsy }`.
/// An array of conditions means AND (all must be true).
pub fn evaluate_condition_table(
    condition: &serde_json::Value,
    data: &serde_json::Value,
) -> bool {
    match condition {
        serde_json::Value::Array(arr) => arr.iter().all(|c| evaluate_condition_table(c, data)),
        serde_json::Value::Object(obj) => {
            let field_name = obj.get("field").and_then(|v| v.as_str()).unwrap_or("");
            let field_val = data.get(field_name).unwrap_or(&serde_json::Value::Null);

            if let Some(eq) = obj.get("equals") {
                return field_val == eq;
            }
            if let Some(neq) = obj.get("not_equals") {
                return field_val != neq;
            }
            if let Some(serde_json::Value::Array(list)) = obj.get("in") {
                return list.contains(field_val);
            }
            if let Some(serde_json::Value::Array(list)) = obj.get("not_in") {
                return !list.contains(field_val);
            }
            if obj.get("is_truthy").and_then(|v| v.as_bool()).unwrap_or(false) {
                return condition_is_truthy(field_val);
            }
            if obj.get("is_falsy").and_then(|v| v.as_bool()).unwrap_or(false) {
                return !condition_is_truthy(field_val);
            }
            true // unknown operator → show
        }
        _ => true,
    }
}

/// Check if a JSON value is "truthy" for display condition evaluation.
fn condition_is_truthy(val: &serde_json::Value) -> bool {
    match val {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Number(_) => true,
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- is_valid_date_format tests ---

    #[test]
    fn test_valid_date_format_date_only() {
        assert!(is_valid_date_format("2024-01-15"));
        assert!(is_valid_date_format("2000-12-31"));
        assert!(is_valid_date_format("1999-06-01"));
    }

    #[test]
    fn test_valid_date_format_datetime_local() {
        assert!(is_valid_date_format("2024-01-15T10:30"));
        assert!(is_valid_date_format("2024-12-31T23:59"));
    }

    #[test]
    fn test_valid_date_format_datetime_seconds() {
        assert!(is_valid_date_format("2024-01-15T10:30:45"));
        assert!(is_valid_date_format("2024-12-31T23:59:59"));
    }

    #[test]
    fn test_valid_date_format_rfc3339() {
        assert!(is_valid_date_format("2024-01-15T10:30:00+00:00"));
        assert!(is_valid_date_format("2024-01-15T10:30:00Z"));
        assert!(is_valid_date_format("2024-01-15T10:30:00-05:00"));
    }

    #[test]
    fn test_valid_date_format_time_only() {
        assert!(is_valid_date_format("10:30"));
        assert!(is_valid_date_format("23:59"));
        assert!(is_valid_date_format("00:00"));
        assert!(is_valid_date_format("10:30:45"));
    }

    #[test]
    fn test_valid_date_format_month_only() {
        assert!(is_valid_date_format("2024-01"));
        assert!(is_valid_date_format("2024-12"));
        assert!(is_valid_date_format("1999-06"));
    }

    #[test]
    fn test_invalid_date_format() {
        assert!(!is_valid_date_format(""));
        assert!(!is_valid_date_format("not-a-date"));
        assert!(!is_valid_date_format("2024"));
        assert!(!is_valid_date_format("2024-1-1"));
        assert!(!is_valid_date_format("01/15/2024"));
        assert!(!is_valid_date_format("2024-13-01")); // invalid month
        assert!(!is_valid_date_format("2024-01-32")); // invalid day
    }

    // --- condition_is_truthy tests ---

    #[test]
    fn test_condition_is_truthy_null() {
        assert!(!condition_is_truthy(&json!(null)));
    }

    #[test]
    fn test_condition_is_truthy_bool() {
        assert!(condition_is_truthy(&json!(true)));
        assert!(!condition_is_truthy(&json!(false)));
    }

    #[test]
    fn test_condition_is_truthy_string() {
        assert!(condition_is_truthy(&json!("hello")));
        assert!(!condition_is_truthy(&json!("")));
    }

    #[test]
    fn test_condition_is_truthy_number() {
        assert!(condition_is_truthy(&json!(0)));
        assert!(condition_is_truthy(&json!(42)));
        assert!(condition_is_truthy(&json!(-1)));
    }

    #[test]
    fn test_condition_is_truthy_array() {
        assert!(condition_is_truthy(&json!([1, 2])));
        assert!(!condition_is_truthy(&json!([])));
    }

    #[test]
    fn test_condition_is_truthy_object() {
        assert!(condition_is_truthy(&json!({"key": "value"})));
        assert!(!condition_is_truthy(&json!({})));
    }

    // --- evaluate_condition_table tests ---

    #[test]
    fn test_condition_equals() {
        let data = json!({"status": "published"});
        let cond = json!({"field": "status", "equals": "published"});
        assert!(evaluate_condition_table(&cond, &data));

        let cond_miss = json!({"field": "status", "equals": "draft"});
        assert!(!evaluate_condition_table(&cond_miss, &data));
    }

    #[test]
    fn test_condition_not_equals() {
        let data = json!({"status": "published"});
        let cond = json!({"field": "status", "not_equals": "draft"});
        assert!(evaluate_condition_table(&cond, &data));

        let cond_miss = json!({"field": "status", "not_equals": "published"});
        assert!(!evaluate_condition_table(&cond_miss, &data));
    }

    #[test]
    fn test_condition_in() {
        let data = json!({"category": "tech"});
        let cond = json!({"field": "category", "in": ["tech", "science"]});
        assert!(evaluate_condition_table(&cond, &data));

        let cond_miss = json!({"field": "category", "in": ["art", "music"]});
        assert!(!evaluate_condition_table(&cond_miss, &data));
    }

    #[test]
    fn test_condition_not_in() {
        let data = json!({"category": "tech"});
        let cond = json!({"field": "category", "not_in": ["art", "music"]});
        assert!(evaluate_condition_table(&cond, &data));

        let cond_miss = json!({"field": "category", "not_in": ["tech", "science"]});
        assert!(!evaluate_condition_table(&cond_miss, &data));
    }

    #[test]
    fn test_condition_is_truthy_op() {
        let data = json!({"featured": true});
        let cond = json!({"field": "featured", "is_truthy": true});
        assert!(evaluate_condition_table(&cond, &data));

        let data_false = json!({"featured": false});
        assert!(!evaluate_condition_table(&cond, &data_false));
    }

    #[test]
    fn test_condition_is_falsy_op() {
        let data = json!({"featured": false});
        let cond = json!({"field": "featured", "is_falsy": true});
        assert!(evaluate_condition_table(&cond, &data));

        let data_true = json!({"featured": true});
        assert!(!evaluate_condition_table(&cond, &data_true));
    }

    #[test]
    fn test_condition_array_and() {
        let data = json!({"status": "published", "featured": true});
        let cond = json!([
            {"field": "status", "equals": "published"},
            {"field": "featured", "is_truthy": true}
        ]);
        assert!(evaluate_condition_table(&cond, &data));

        let data_fail = json!({"status": "draft", "featured": true});
        assert!(!evaluate_condition_table(&cond, &data_fail));
    }

    #[test]
    fn test_condition_missing_field() {
        let data = json!({"status": "published"});
        let cond = json!({"field": "nonexistent", "equals": "something"});
        assert!(!evaluate_condition_table(&cond, &data));
    }

    #[test]
    fn test_condition_unknown_operator_shows() {
        let data = json!({"status": "published"});
        let cond = json!({"field": "status"});
        // Unknown operator → show (returns true)
        assert!(evaluate_condition_table(&cond, &data));
    }

    #[test]
    fn test_condition_non_object_non_array_shows() {
        let data = json!({"status": "published"});
        // Non-object, non-array → true
        assert!(evaluate_condition_table(&json!("string"), &data));
        assert!(evaluate_condition_table(&json!(42), &data));
        assert!(evaluate_condition_table(&json!(null), &data));
    }

    // --- validate_fields_inner tests ---

    #[test]
    fn test_validate_required_field_empty_string() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            required: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.errors.len(), 1);
        assert!(err.errors[0].message.contains("required"));
    }

    #[test]
    fn test_validate_required_field_null() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            required: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!(null));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_required_skipped_for_drafts() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            required: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, true);
        assert!(result.is_ok(), "Drafts should skip required checks");
    }

    #[test]
    fn test_validate_required_join_field_empty_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Relationship,
            required: true,
            relationship: Some(crate::core::field::RelationshipConfig {
                collection: "tags".to_string(),
                has_many: true,
                max_depth: None,
                polymorphic: vec![],
            }),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!([]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("required"));
    }

    #[test]
    fn test_validate_required_join_field_non_empty_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Relationship,
            required: true,
            relationship: Some(crate::core::field::RelationshipConfig {
                collection: "tags".to_string(),
                has_many: true,
                max_depth: None,
                polymorphic: vec![],
            }),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(["t1", "t2"]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_group_subfield_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "seo".to_string(),
            field_type: FieldType::Group,
            fields: vec![FieldDefinition {
                name: "title".to_string(),
                required: true,
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_min_rows() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            min_rows: Some(2),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"label": "one"}]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at least 2"));
    }

    #[test]
    fn test_validate_max_rows() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            max_rows: Some(1),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"a": 1}, {"a": 2}]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at most 1"));
    }

    #[test]
    fn test_validate_array_sub_field_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            fields: vec![FieldDefinition {
                name: "label".to_string(),
                required: true,
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"label": ""}]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.errors[0].field.contains("items[0][label]"));
    }

    #[test]
    fn test_validate_blocks_sub_field_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "content".to_string(),
            field_type: FieldType::Blocks,
            blocks: vec![crate::core::field::BlockDefinition {
                block_type: "text".to_string(),
                fields: vec![FieldDefinition {
                    name: "body".to_string(),
                    required: true,
                    ..Default::default()
                }],
                label: None,
                label_field: None,
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("content".to_string(), json!([{"_block_type": "text", "body": ""}]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].field.contains("content[0][body]"));
    }

    #[test]
    fn test_validate_date_format_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, d TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "d".to_string(),
            field_type: FieldType::Date,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("d".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_date_format_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, d TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "d".to_string(),
            field_type: FieldType::Date,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("d".to_string(), json!("2024-01-15"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_custom_validate_function_returns_error() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = {
                validate_test = function(value, ctx)
                    if value == "bad" then
                        return "value cannot be bad"
                    end
                    return true
                end
            }
        "#).exec().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            validate: Some("validators.validate_test".to_string()),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("bad"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("cannot be bad"));
    }

    #[test]
    fn test_validate_custom_validate_function_returns_false() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_fail = function(value, ctx)
                return false
            end
        "#).exec().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            validate: Some("validators.validate_fail".to_string()),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("anything"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].message, "validation failed");
    }

    #[test]
    fn test_validate_custom_validate_function_returns_true() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_ok = function(value, ctx)
                return true
            end
        "#).exec().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            validate: Some("validators.validate_ok".to_string()),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("good"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_unique_check() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT);
             INSERT INTO test (id, email) VALUES ('existing', 'taken@test.com');"
        ).unwrap();
        let fields = vec![FieldDefinition {
            name: "email".to_string(),
            unique: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("taken@test.com"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("unique"));
    }

    #[test]
    fn test_validate_unique_check_excludes_self() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT);
             INSERT INTO test (id, email) VALUES ('self', 'me@test.com');"
        ).unwrap();
        let fields = vec![FieldDefinition {
            name: "email".to_string(),
            unique: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("me@test.com"));
        // exclude_id = "self" means we're updating ourselves, so this is fine
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", Some("self"), false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_required_skipped_on_update_absent_field() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            required: true,
            ..Default::default()
        }];
        // On update (exclude_id set), absent field = partial update, should not fail
        let data = HashMap::new();
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", Some("p1"), false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_checkbox_not_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, active INTEGER)").unwrap();
        let fields = vec![FieldDefinition {
            name: "active".to_string(),
            field_type: FieldType::Checkbox,
            required: true,
            ..Default::default()
        }];
        // Checkbox absent = false, which is valid even when required
        let data = HashMap::new();
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok());
    }

    // --- run_validate_function_inner tests ---

    #[test]
    fn test_run_validate_function_nil_means_valid() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = {
                validate_nil = function(value, ctx)
                    return nil
                end
            }
        "#).exec().unwrap();
        let data = HashMap::new();
        let result = run_validate_function_inner(&lua, "validators.validate_nil", &json!("test"), &data, "test", "name").unwrap();
        assert!(result.is_none());
    }

    // --- nested validation in sub-fields ---

    #[test]
    fn test_validate_nested_array_in_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "outer".to_string(),
            field_type: FieldType::Array,
            fields: vec![FieldDefinition {
                name: "inner".to_string(),
                field_type: FieldType::Array,
                fields: vec![FieldDefinition {
                    name: "value".to_string(),
                    required: true,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("outer".to_string(), json!([
            {"inner": [{"value": ""}]}
        ]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.errors[0].field.contains("outer[0][inner][0][value]"));
    }

    #[test]
    fn test_validate_group_inside_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            fields: vec![FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![FieldDefinition {
                    name: "title".to_string(),
                    required: true,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([
            {"meta__title": ""}
        ]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.errors[0].field.contains("items[0][meta__title]"));
    }

    #[test]
    fn test_validate_date_inside_array_subfield() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "events".to_string(),
            field_type: FieldType::Array,
            fields: vec![FieldDefinition {
                name: "date".to_string(),
                field_type: FieldType::Date,
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("events".to_string(), json!([
            {"date": "not-a-date"}
        ]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_custom_validate_in_array_subfield() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_sub = function(value, ctx)
                if value == "invalid" then
                    return "sub-field invalid"
                end
                return true
            end
        "#).exec().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            fields: vec![FieldDefinition {
                name: "val".to_string(),
                validate: Some("validators.validate_sub".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([
            {"val": "invalid"}
        ]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("sub-field invalid"));
    }

    #[test]
    fn test_validate_date_in_group_inside_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            fields: vec![FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![FieldDefinition {
                    name: "publish_date".to_string(),
                    field_type: FieldType::Date,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([
            {"meta__publish_date": "bad-date"}
        ]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_custom_function_in_group_inside_array() {
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_group_sub = function(value, ctx)
                return "group validation error"
            end
        "#).exec().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            fields: vec![FieldDefinition {
                name: "meta".to_string(),
                field_type: FieldType::Group,
                fields: vec![FieldDefinition {
                    name: "slug".to_string(),
                    validate: Some("validators.validate_group_sub".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([
            {"meta__slug": "test-slug"}
        ]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("group validation error"));
    }

    // ── validation inside layout fields (collapsible, tabs) ─────────────

    #[test]
    fn test_validate_required_inside_collapsible() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, notes TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "extra".to_string(),
            field_type: FieldType::Collapsible,
            fields: vec![FieldDefinition {
                name: "notes".to_string(),
                required: true,
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("notes".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "notes");
    }

    #[test]
    fn test_validate_required_inside_tabs() {
        use crate::core::field::FieldTab;
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![
                FieldTab {
                    label: "Content".to_string(),
                    description: None,
                    fields: vec![FieldDefinition {
                        name: "body".to_string(),
                        required: true,
                        ..Default::default()
                    }],
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("body".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "body");
    }

    #[test]
    fn test_validate_group_inside_tabs_required() {
        use crate::core::field::FieldTab;
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![
                FieldTab {
                    label: "SEO".to_string(),
                    description: None,
                    fields: vec![FieldDefinition {
                        name: "seo".to_string(),
                        field_type: FieldType::Group,
                        fields: vec![FieldDefinition {
                            name: "title".to_string(),
                            required: true,
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_group_inside_collapsible_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, seo__title TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "extra".to_string(),
            field_type: FieldType::Collapsible,
            fields: vec![FieldDefinition {
                name: "seo".to_string(),
                field_type: FieldType::Group,
                fields: vec![FieldDefinition {
                    name: "title".to_string(),
                    required: true,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("seo__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].field, "seo__title");
    }

    #[test]
    fn test_validate_date_inside_tabs() {
        use crate::core::field::FieldTab;
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, publish_date TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![
                FieldTab {
                    label: "Meta".to_string(),
                    description: None,
                    fields: vec![FieldDefinition {
                        name: "publish_date".to_string(),
                        field_type: FieldType::Date,
                        ..Default::default()
                    }],
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("publish_date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_unique_inside_tabs() {
        use crate::core::field::FieldTab;
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, slug TEXT);
             INSERT INTO test (id, slug) VALUES ('existing', 'taken');"
        ).unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![
                FieldTab {
                    label: "Meta".to_string(),
                    description: None,
                    fields: vec![FieldDefinition {
                        name: "slug".to_string(),
                        unique: true,
                        ..Default::default()
                    }],
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("slug".to_string(), json!("taken"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("unique"));
    }

    #[test]
    fn test_validate_custom_function_inside_tabs() {
        use crate::core::field::FieldTab;
        let lua = mlua::Lua::new();
        lua.load(r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_tabs_field = function(value, ctx)
                if value == "bad" then return "tabs validation error" end
                return true
            end
        "#).exec().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![
                FieldTab {
                    label: "Content".to_string(),
                    description: None,
                    fields: vec![FieldDefinition {
                        name: "body".to_string(),
                        validate: Some("validators.validate_tabs_field".to_string()),
                        ..Default::default()
                    }],
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("body".to_string(), json!("bad"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("tabs validation error"));
    }

    #[test]
    fn test_validate_deeply_nested_tabs_collapsible_group() {
        use crate::core::field::FieldTab;
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, og__title TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![
                FieldTab {
                    label: "Advanced".to_string(),
                    description: None,
                    fields: vec![
                        FieldDefinition {
                            name: "advanced".to_string(),
                            field_type: FieldType::Collapsible,
                            fields: vec![
                                FieldDefinition {
                                    name: "og".to_string(),
                                    field_type: FieldType::Group,
                                    fields: vec![FieldDefinition {
                                        name: "title".to_string(),
                                        required: true,
                                        ..Default::default()
                                    }],
                                    ..Default::default()
                                },
                            ],
                            ..Default::default()
                        },
                    ],
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("og__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Deeply nested Group inside Collapsible inside Tabs should validate");
        assert_eq!(result.unwrap_err().errors[0].field, "og__title");
    }

    #[test]
    fn test_validate_layout_fields_skipped_for_drafts() {
        use crate::core::field::FieldTab;
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, body TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "layout".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![
                FieldTab {
                    label: "Content".to_string(),
                    description: None,
                    fields: vec![FieldDefinition {
                        name: "body".to_string(),
                        required: true,
                        ..Default::default()
                    }],
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("body".to_string(), json!(""));
        // is_draft = true should skip all required checks
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, true);
        assert!(result.is_ok(), "Draft saves should skip required checks in layout fields");
    }

    // ── Group containing layout fields (the former terminal-node bug) ─────

    #[test]
    fn test_validate_group_containing_row_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, meta__title TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "meta".to_string(),
            field_type: FieldType::Group,
            fields: vec![
                FieldDefinition {
                    name: "r".to_string(),
                    field_type: FieldType::Row,
                    fields: vec![FieldDefinition {
                        name: "title".to_string(),
                        required: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("meta__title".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Group→Row: required field should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "meta__title");
    }

    #[test]
    fn test_validate_group_containing_collapsible_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, seo__robots TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "seo".to_string(),
            field_type: FieldType::Group,
            fields: vec![
                FieldDefinition {
                    name: "c".to_string(),
                    field_type: FieldType::Collapsible,
                    fields: vec![FieldDefinition {
                        name: "robots".to_string(),
                        required: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("seo__robots".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Group→Collapsible: required field should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "seo__robots");
    }

    #[test]
    fn test_validate_group_containing_tabs_required() {
        use crate::core::field::FieldTab;
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, settings__theme TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "settings".to_string(),
            field_type: FieldType::Group,
            fields: vec![
                FieldDefinition {
                    name: "t".to_string(),
                    field_type: FieldType::Tabs,
                    tabs: vec![FieldTab {
                        label: "General".to_string(),
                        description: None,
                        fields: vec![FieldDefinition {
                            name: "theme".to_string(),
                            required: true,
                            ..Default::default()
                        }],
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("settings__theme".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Group→Tabs: required field should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "settings__theme");
    }

    #[test]
    fn test_validate_group_tabs_group_three_levels_required() {
        use crate::core::field::FieldTab;
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, outer__inner__deep TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "outer".to_string(),
            field_type: FieldType::Group,
            fields: vec![
                FieldDefinition {
                    name: "t".to_string(),
                    field_type: FieldType::Tabs,
                    tabs: vec![FieldTab {
                        label: "Tab".to_string(),
                        description: None,
                        fields: vec![
                            FieldDefinition {
                                name: "inner".to_string(),
                                field_type: FieldType::Group,
                                fields: vec![FieldDefinition {
                                    name: "deep".to_string(),
                                    required: true,
                                    ..Default::default()
                                }],
                                ..Default::default()
                            },
                        ],
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("outer__inner__deep".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Group→Tabs→Group: required field should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "outer__inner__deep");
    }

    #[test]
    fn test_validate_group_containing_tabs_unique() {
        use crate::core::field::FieldTab;
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE test (id TEXT PRIMARY KEY, config__slug TEXT);
             INSERT INTO test (id, config__slug) VALUES ('existing', 'taken');"
        ).unwrap();
        let fields = vec![FieldDefinition {
            name: "config".to_string(),
            field_type: FieldType::Group,
            fields: vec![
                FieldDefinition {
                    name: "t".to_string(),
                    field_type: FieldType::Tabs,
                    tabs: vec![FieldTab {
                        label: "Tab".to_string(),
                        description: None,
                        fields: vec![FieldDefinition {
                            name: "slug".to_string(),
                            unique: true,
                            ..Default::default()
                        }],
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("config__slug".to_string(), json!("taken"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Group→Tabs: unique field should fail on duplicate");
        assert_eq!(result.unwrap_err().errors[0].field, "config__slug");
    }

    #[test]
    fn test_validate_group_containing_row_date_format() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, meta__date TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "meta".to_string(),
            field_type: FieldType::Group,
            fields: vec![
                FieldDefinition {
                    name: "r".to_string(),
                    field_type: FieldType::Row,
                    fields: vec![FieldDefinition {
                        name: "date".to_string(),
                        field_type: FieldType::Date,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("meta__date".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Group→Row: invalid date should fail");
        assert_eq!(result.unwrap_err().errors[0].field, "meta__date");
    }

    #[test]
    fn test_validate_group_containing_row_valid_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, meta__title TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "meta".to_string(),
            field_type: FieldType::Group,
            fields: vec![
                FieldDefinition {
                    name: "r".to_string(),
                    field_type: FieldType::Row,
                    fields: vec![FieldDefinition {
                        name: "title".to_string(),
                        required: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("meta__title".to_string(), json!("Valid Title"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Group→Row: valid data should pass");
    }

    // ── min_length / max_length validation ──────────────────────────────

    #[test]
    fn test_validate_min_length_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            min_length: Some(5),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("ab"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at least 5 characters"));
    }

    #[test]
    fn test_validate_min_length_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            min_length: Some(3),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("hello"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_max_length_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            max_length: Some(5),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("toolongvalue"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at most 5 characters"));
    }

    #[test]
    fn test_validate_max_length_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            max_length: Some(10),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("short"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_min_max_length_skipped_for_empty() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "name".to_string(),
            min_length: Some(5),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "min_length should not trigger on empty values");
    }

    // ── min / max (number) validation ───────────────────────────────────

    #[test]
    fn test_validate_number_min_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, score REAL)").unwrap();
        let fields = vec![FieldDefinition {
            name: "score".to_string(),
            field_type: FieldType::Number,
            min: Some(0.0),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("score".to_string(), json!("-5"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at least 0"));
    }

    #[test]
    fn test_validate_number_max_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, score REAL)").unwrap();
        let fields = vec![FieldDefinition {
            name: "score".to_string(),
            field_type: FieldType::Number,
            max: Some(100.0),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("score".to_string(), json!("150"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at most 100"));
    }

    #[test]
    fn test_validate_number_min_max_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, score REAL)").unwrap();
        let fields = vec![FieldDefinition {
            name: "score".to_string(),
            field_type: FieldType::Number,
            min: Some(0.0),
            max: Some(100.0),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("score".to_string(), json!("50"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_number_min_max_skipped_for_empty() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, score REAL)").unwrap();
        let fields = vec![FieldDefinition {
            name: "score".to_string(),
            field_type: FieldType::Number,
            min: Some(10.0),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("score".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "min/max should not trigger on empty values");
    }

    #[test]
    fn test_validate_number_json_number_value() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, score REAL)").unwrap();
        let fields = vec![FieldDefinition {
            name: "score".to_string(),
            field_type: FieldType::Number,
            min: Some(0.0),
            max: Some(10.0),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("score".to_string(), json!(15));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at most 10"));
    }

    // ── Email format validation ─────────────────────────────────────────

    #[test]
    fn test_validate_email_format_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "email".to_string(),
            field_type: FieldType::Email,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("user@example.com"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_email_format_invalid_no_at() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "email".to_string(),
            field_type: FieldType::Email,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("not-an-email"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid email"));
    }

    #[test]
    fn test_validate_email_format_invalid_no_domain() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "email".to_string(),
            field_type: FieldType::Email,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!("user@"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_email_format_skipped_for_empty() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, email TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "email".to_string(),
            field_type: FieldType::Email,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("email".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Email validation should skip empty values");
    }

    // ── Select option validation ────────────────────────────────────────

    #[test]
    fn test_validate_select_option_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, color TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "color".to_string(),
            field_type: FieldType::Select,
            options: vec![
                crate::core::field::SelectOption {
                    label: crate::core::field::LocalizedString::Plain("Red".to_string()),
                    value: "red".to_string(),
                },
                crate::core::field::SelectOption {
                    label: crate::core::field::LocalizedString::Plain("Blue".to_string()),
                    value: "blue".to_string(),
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("color".to_string(), json!("red"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_select_option_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, color TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "color".to_string(),
            field_type: FieldType::Select,
            options: vec![
                crate::core::field::SelectOption {
                    label: crate::core::field::LocalizedString::Plain("Red".to_string()),
                    value: "red".to_string(),
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("color".to_string(), json!("green"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("invalid option"));
    }

    #[test]
    fn test_validate_select_option_empty_value_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, color TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "color".to_string(),
            field_type: FieldType::Select,
            options: vec![
                crate::core::field::SelectOption {
                    label: crate::core::field::LocalizedString::Plain("Red".to_string()),
                    value: "red".to_string(),
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("color".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Empty select value should pass (not required)");
    }

    // ── is_valid_email_format tests ─────────────────────────────────────

    #[test]
    fn test_email_format_valid_addresses() {
        assert!(is_valid_email_format("user@example.com"));
        assert!(is_valid_email_format("a@b.c"));
        assert!(is_valid_email_format("test+tag@domain.org"));
        assert!(is_valid_email_format("user.name@sub.domain.com"));
    }

    #[test]
    fn test_email_format_invalid_addresses() {
        assert!(!is_valid_email_format(""));
        assert!(!is_valid_email_format("no-at-sign"));
        assert!(!is_valid_email_format("@no-local.com"));
        assert!(!is_valid_email_format("user@"));
        assert!(!is_valid_email_format("user@nodot"));
        assert!(!is_valid_email_format("user @space.com"));
        assert!(!is_valid_email_format("user@ space.com"));
    }

    // ── has_many select validation tests ─────────────────────────────────

    #[test]
    fn test_validate_has_many_select_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Select,
            has_many: true,
            options: vec![
                crate::core::field::SelectOption {
                    label: crate::core::field::LocalizedString::Plain("Red".to_string()),
                    value: "red".to_string(),
                },
                crate::core::field::SelectOption {
                    label: crate::core::field::LocalizedString::Plain("Blue".to_string()),
                    value: "blue".to_string(),
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["red","blue"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Valid has_many select values should pass");
    }

    #[test]
    fn test_validate_has_many_select_invalid_option() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Select,
            has_many: true,
            options: vec![
                crate::core::field::SelectOption {
                    label: crate::core::field::LocalizedString::Plain("Red".to_string()),
                    value: "red".to_string(),
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["red","invalid"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("invalid option"));
    }

    #[test]
    fn test_validate_has_many_select_empty_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Select,
            has_many: true,
            options: vec![
                crate::core::field::SelectOption {
                    label: crate::core::field::LocalizedString::Plain("Red".to_string()),
                    value: "red".to_string(),
                },
            ],
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!("[]"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Empty array for has_many select should pass");
    }

    // ── has_many text/number validation tests ──────────────────────────

    #[test]
    fn test_validate_has_many_text_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Text,
            has_many: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["rust","lua","python"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Valid has_many text values should pass");
    }

    #[test]
    fn test_validate_has_many_text_min_length_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Text,
            has_many: true,
            min_length: Some(3),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["rust","ab"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at least 3 characters"));
    }

    #[test]
    fn test_validate_has_many_text_max_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Text,
            has_many: true,
            max_rows: Some(2),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["a","b","c"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at most 2 values"));
    }

    #[test]
    fn test_validate_has_many_number_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, scores TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "scores".to_string(),
            field_type: FieldType::Number,
            has_many: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("scores".to_string(), json!(r#"["10","20","30"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Valid has_many number values should pass");
    }

    #[test]
    fn test_validate_has_many_number_max_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, scores TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "scores".to_string(),
            field_type: FieldType::Number,
            has_many: true,
            max: Some(50.0),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("scores".to_string(), json!(r#"["10","75"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at most 50"));
    }

    // ── has_many regression tests (bug fixes) ─────────────────────────────

    #[test]
    fn test_has_many_text_required_empty_array_fails() {
        // Bug fix: "[]" should be treated as empty for required has_many fields
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Text,
            has_many: true,
            required: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!("[]"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Empty array should fail required check");
        assert!(result.unwrap_err().errors[0].message.contains("required"));
    }

    #[test]
    fn test_has_many_text_required_with_values_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Text,
            has_many: true,
            required: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["rust"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Non-empty array should pass required check");
    }

    #[test]
    fn test_has_many_text_max_length_not_applied_to_json_string() {
        // Bug fix: max_length should validate per-value, not the JSON string representation
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "tags".to_string(),
            field_type: FieldType::Text,
            has_many: true,
            max_length: Some(10),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        // JSON string is 23 chars, but each value is only 8 chars — should pass
        data.insert("tags".to_string(), json!(r#"["abcdefgh","abcdefgh"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "max_length should check per-value, not JSON string length");
    }

    // ── date bounds validation tests ─────────────────────────────────────

    #[test]
    fn test_validate_date_min_date_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, start_date TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "start_date".to_string(),
            field_type: FieldType::Date,
            min_date: Some("2024-01-01".to_string()),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("start_date".to_string(), json!("2024-06-15T12:00:00.000Z"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Date after min_date should pass");
    }

    #[test]
    fn test_validate_date_min_date_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, start_date TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "start_date".to_string(),
            field_type: FieldType::Date,
            min_date: Some("2024-06-01".to_string()),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("start_date".to_string(), json!("2024-01-15T12:00:00.000Z"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("on or after"));
    }

    #[test]
    fn test_validate_date_max_date_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, end_date TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "end_date".to_string(),
            field_type: FieldType::Date,
            max_date: Some("2025-12-31".to_string()),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("end_date".to_string(), json!("2026-03-15T12:00:00.000Z"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("on or before"));
    }

    #[test]
    fn test_validate_date_bounds_empty_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, d TEXT)").unwrap();
        let fields = vec![FieldDefinition {
            name: "d".to_string(),
            field_type: FieldType::Date,
            min_date: Some("2024-01-01".to_string()),
            max_date: Some("2025-12-31".to_string()),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("d".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Empty date with bounds should pass (not required)");
    }

    #[test]
    fn join_field_skipped_in_validation() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        // Join field with required=true — should still be skipped (virtual, no data)
        let fields = vec![FieldDefinition {
            name: "posts".to_string(),
            field_type: FieldType::Join,
            required: true,
            join: Some(crate::core::field::JoinConfig {
                collection: "posts".to_string(),
                on: "author".to_string(),
            }),
            ..Default::default()
        }];
        let data = HashMap::new(); // No data submitted for join field
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Join field should be skipped entirely during validation");
    }
}
