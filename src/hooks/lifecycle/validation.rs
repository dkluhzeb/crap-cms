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

    for field in fields {
        let value = data.get(&field.name);
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
                        field: field.name.clone(),
                        message: format!("{} is required", field.name),
                    });
                }
            } else if is_empty {
                errors.push(FieldError {
                    field: field.name.clone(),
                    message: format!("{} is required", field.name),
                });
            }
        }

        // Validate Group sub-fields (stored as group__subfield keys at top level)
        if field.field_type == FieldType::Group && !is_draft {
            for gsf in &field.fields {
                let key = format!("{}__{}", field.name, gsf.name);
                let gv = data.get(&key);
                let g_empty = match gv {
                    None => true,
                    Some(serde_json::Value::Null) => true,
                    Some(serde_json::Value::String(s)) => s.is_empty(),
                    _ => false,
                };
                if gsf.required && g_empty && gsf.field_type != FieldType::Checkbox {
                    errors.push(FieldError {
                        field: key,
                        message: format!("{} is required", gsf.name),
                    });
                }
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
                        field: field.name.clone(),
                        message: format!("{} requires at least {} item(s)", field.name, min),
                    });
                }
            }
            if let Some(max) = field.max_rows {
                if row_count > max {
                    errors.push(FieldError {
                        field: field.name.clone(),
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
                        lua, sub_fields, row_obj, &field.name, idx, table, &mut errors,
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
            match query::count_where_field_eq(conn, table, &field.name, &value_str, exclude_id) {
                Ok(count) if count > 0 => {
                    errors.push(FieldError {
                        field: field.name.clone(),
                        message: format!("{} must be unique", field.name),
                    });
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("Unique check failed for {}.{}: {}", table, field.name, e);
                }
            }
        }

        // Date format validation (only if non-empty)
        if field.field_type == FieldType::Date && !is_empty {
            if let Some(serde_json::Value::String(s)) = value {
                if !is_valid_date_format(s) {
                    errors.push(FieldError {
                        field: field.name.clone(),
                        message: format!("{} is not a valid date format", field.name),
                    });
                }
            }
        }

        // Custom validate function (Lua)
        if let Some(ref validate_ref) = field.validate {
            if let Some(val) = value {
                match run_validate_function_inner(lua, validate_ref, val, data, table, &field.name) {
                    Ok(Some(err_msg)) => {
                        errors.push(FieldError {
                            field: field.name.clone(),
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

    if errors.is_empty() {
        Ok(())
    } else {
        Err(ValidationError { errors })
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
}
