use std::collections::HashMap;
use crate::core::field::FieldDefinition;
use crate::core::validate::FieldError;

/// Validate min / max bounds for number fields.
/// Skipped for has_many fields (validated per-element in `check_has_many_elements`).
pub(crate) fn check_numeric_bounds(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&serde_json::Value>,
    is_empty: bool,
    errors: &mut Vec<FieldError>,
) {
    if is_empty || field.has_many || (field.min.is_none() && field.max.is_none()) {
        return;
    }
    let num_val = match value {
        Some(serde_json::Value::Number(n)) => n.as_f64(),
        Some(serde_json::Value::String(s)) => s.parse::<f64>().ok(),
        _ => None,
    };
    if let Some(v) = num_val {
        if let Some(min_val) = field.min {
            if v < min_val {
                errors.push(FieldError::with_key(data_key.to_owned(), format!("{} must be at least {}", field.name, min_val), "validation.min_value", HashMap::from([("field".to_string(), field.name.clone()), ("min".to_string(), min_val.to_string())])));
            }
        }
        if let Some(max_val) = field.max {
            if v > max_val {
                errors.push(FieldError::with_key(data_key.to_owned(), format!("{} must be at most {}", field.name, max_val), "validation.max_value", HashMap::from([("field".to_string(), field.name.clone()), ("max".to_string(), max_val.to_string())])));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::field::{FieldDefinition, FieldType};
    use crate::hooks::lifecycle::validation::validate_fields_inner;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_validate_number_min_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, score REAL)").unwrap();
        let fields = vec![FieldDefinition::builder("score", FieldType::Number).min(0.0).build()];
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
        let fields = vec![FieldDefinition::builder("score", FieldType::Number).max(100.0).build()];
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
        let fields = vec![FieldDefinition::builder("score", FieldType::Number).min(0.0).max(100.0).build()];
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
        let fields = vec![FieldDefinition::builder("score", FieldType::Number).min(10.0).build()];
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
        let fields = vec![FieldDefinition::builder("score", FieldType::Number).min(0.0).max(10.0).build()];
        let mut data = HashMap::new();
        data.insert("score".to_string(), json!(15));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at most 10"));
    }
}
