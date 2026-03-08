use std::collections::HashMap;

use mlua::Lua;

use crate::core::field::FieldDefinition;
use crate::core::validate::FieldError;

use super::super::custom::run_validate_function_inner;

/// Run custom Lua validate function on a field value.
pub(crate) fn check_custom_validate(
    lua: &Lua,
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&serde_json::Value>,
    data: &HashMap<String, serde_json::Value>,
    table: &str,
    errors: &mut Vec<FieldError>,
) {
    let validate_ref = match field.validate {
        Some(ref v) => v,
        None => return,
    };
    let val = match value {
        Some(v) => v,
        None => return,
    };
    match run_validate_function_inner(lua, validate_ref, val, data, table, &field.name) {
        Ok(Some(err_msg)) => {
            errors.push(FieldError::new(data_key.to_owned(), err_msg));
        }
        Ok(None) => {} // valid
        Err(e) => {
            tracing::warn!("Validate function '{}' error: {}", validate_ref, e);
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
}
