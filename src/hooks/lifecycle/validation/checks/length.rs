use crate::core::field::FieldDefinition;
use crate::core::validate::FieldError;

/// Validate min_length / max_length for text/textarea fields.
/// Skipped for has_many fields (validated per-element in `check_has_many_elements`).
pub(crate) fn check_length_bounds(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&serde_json::Value>,
    is_empty: bool,
    errors: &mut Vec<FieldError>,
) {
    if is_empty || field.has_many || (field.min_length.is_none() && field.max_length.is_none()) {
        return;
    }
    if let Some(serde_json::Value::String(s)) = value {
        let len = s.len();
        if let Some(min_len) = field.min_length {
            if len < min_len {
                errors.push(FieldError::new(data_key.to_owned(), format!("{} must be at least {} characters", field.name, min_len)));
            }
        }
        if let Some(max_len) = field.max_length {
            if len > max_len {
                errors.push(FieldError::new(data_key.to_owned(), format!("{} must be at most {} characters", field.name, max_len)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::field::FieldDefinition;
    use crate::hooks::lifecycle::validation::validate_fields_inner;
    use serde_json::json;
    use std::collections::HashMap;

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
}
