use std::collections::HashMap;
use crate::core::field::{FieldDefinition, FieldType};
use crate::core::validate::FieldError;

/// Validate individual values within a has_many text/number JSON array.
/// Checks count bounds (min_rows/max_rows) and per-element constraints.
pub(crate) fn check_has_many_elements(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&serde_json::Value>,
    is_empty: bool,
    errors: &mut Vec<FieldError>,
) {
    if (field.field_type != FieldType::Text && field.field_type != FieldType::Number)
        || !field.has_many || is_empty
    {
        return;
    }
    if let Some(serde_json::Value::String(s)) = value {
        if let Ok(values) = serde_json::from_str::<Vec<String>>(s) {
            // Validate count with min_rows/max_rows
            if let Some(min_rows) = field.min_rows {
                if values.len() < min_rows {
                    errors.push(FieldError::with_key(data_key.to_owned(), format!("{} must have at least {} values", field.name, min_rows), "validation.has_many_min_rows", HashMap::from([("field".to_string(), field.name.clone()), ("min".to_string(), min_rows.to_string())])));
                }
            }
            if let Some(max_rows) = field.max_rows {
                if values.len() > max_rows {
                    errors.push(FieldError::with_key(data_key.to_owned(), format!("{} must have at most {} values", field.name, max_rows), "validation.has_many_max_rows", HashMap::from([("field".to_string(), field.name.clone()), ("max".to_string(), max_rows.to_string())])));
                }
            }
            // Validate each value
            for v in &values {
                if field.field_type == FieldType::Text {
                    if let Some(min_len) = field.min_length {
                        if v.len() < min_len {
                            errors.push(FieldError::with_key(data_key.to_owned(), format!("{}: '{}' must be at least {} characters", field.name, v, min_len), "validation.has_many_min_length", HashMap::from([("field".to_string(), field.name.clone()), ("value".to_string(), v.clone()), ("min".to_string(), min_len.to_string())])));
                            break;
                        }
                    }
                    if let Some(max_len) = field.max_length {
                        if v.len() > max_len {
                            errors.push(FieldError::with_key(data_key.to_owned(), format!("{}: '{}' must be at most {} characters", field.name, v, max_len), "validation.has_many_max_length", HashMap::from([("field".to_string(), field.name.clone()), ("value".to_string(), v.clone()), ("max".to_string(), max_len.to_string())])));
                            break;
                        }
                    }
                } else if field.field_type == FieldType::Number {
                    if let Ok(num) = v.parse::<f64>() {
                        if let Some(min_val) = field.min {
                            if num < min_val {
                                errors.push(FieldError::with_key(data_key.to_owned(), format!("{}: {} must be at least {}", field.name, v, min_val), "validation.has_many_min_value", HashMap::from([("field".to_string(), field.name.clone()), ("value".to_string(), v.clone()), ("min".to_string(), min_val.to_string())])));
                                break;
                            }
                        }
                        if let Some(max_val) = field.max {
                            if num > max_val {
                                errors.push(FieldError::with_key(data_key.to_owned(), format!("{}: {} must be at most {}", field.name, v, max_val), "validation.has_many_max_value", HashMap::from([("field".to_string(), field.name.clone()), ("value".to_string(), v.clone()), ("max".to_string(), max_val.to_string())])));
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::field::{FieldDefinition, FieldType, LocalizedString, SelectOption};
    use crate::hooks::lifecycle::validation::validate_fields_inner;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_validate_has_many_select_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition::builder("tags", FieldType::Select)
            .has_many(true)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
                SelectOption::new(LocalizedString::Plain("Blue".to_string()), "blue"),
            ])
            .build()];
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
        let fields = vec![FieldDefinition::builder("tags", FieldType::Select)
            .has_many(true)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
            ])
            .build()];
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
        let fields = vec![FieldDefinition::builder("tags", FieldType::Select)
            .has_many(true)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
            ])
            .build()];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!("[]"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Empty array for has_many select should pass");
    }

    #[test]
    fn test_validate_has_many_text_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition::builder("tags", FieldType::Text).has_many(true).build()];
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
        let fields = vec![FieldDefinition::builder("tags", FieldType::Text).has_many(true).min_length(3).build()];
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
        let fields = vec![FieldDefinition::builder("tags", FieldType::Text).has_many(true).max_rows(2).build()];
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
        let fields = vec![FieldDefinition::builder("scores", FieldType::Number).has_many(true).build()];
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
        let fields = vec![FieldDefinition::builder("scores", FieldType::Number).has_many(true).max(50.0).build()];
        let mut data = HashMap::new();
        data.insert("scores".to_string(), json!(r#"["10","75"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at most 50"));
    }

    #[test]
    fn test_has_many_text_required_empty_array_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition::builder("tags", FieldType::Text).has_many(true).required(true).build()];
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
        let fields = vec![FieldDefinition::builder("tags", FieldType::Text).has_many(true).required(true).build()];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["rust"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Non-empty array should pass required check");
    }

    #[test]
    fn test_has_many_text_max_length_not_applied_to_json_string() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition::builder("tags", FieldType::Text).has_many(true).max_length(10).build()];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["abcdefgh","abcdefgh"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "max_length should check per-value, not JSON string length");
    }

    #[test]
    fn test_validate_has_many_text_min_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition::builder("tags", FieldType::Text).has_many(true).min_rows(3).build()];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["a","b"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "has_many text with fewer items than min_rows should fail");
        assert!(result.unwrap_err().errors[0].message.contains("at least 3"));
    }

    #[test]
    fn test_validate_has_many_number_min_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, scores TEXT)").unwrap();
        let fields = vec![FieldDefinition::builder("scores", FieldType::Number).has_many(true).min_rows(2).build()];
        let mut data = HashMap::new();
        data.insert("scores".to_string(), json!(r#"["10"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "has_many number with fewer items than min_rows should fail");
        assert!(result.unwrap_err().errors[0].message.contains("at least 2"));
    }

    #[test]
    fn test_validate_has_many_number_min_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, scores TEXT)").unwrap();
        let fields = vec![FieldDefinition::builder("scores", FieldType::Number).has_many(true).min(5.0).build()];
        let mut data = HashMap::new();
        data.insert("scores".to_string(), json!(r#"["10","2"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "has_many number with value below min should fail");
        assert!(result.unwrap_err().errors[0].message.contains("at least 5"));
    }

    #[test]
    fn test_validate_has_many_text_max_length_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)").unwrap();
        let fields = vec![FieldDefinition::builder("tags", FieldType::Text).has_many(true).max_length(3).build()];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["ab","toolong"]"#));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "has_many text with value exceeding max_length should fail");
        assert!(result.unwrap_err().errors[0].message.contains("at most 3 characters"));
    }
}
