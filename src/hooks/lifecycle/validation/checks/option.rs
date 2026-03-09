use std::collections::HashMap;
use crate::core::field::{FieldDefinition, FieldType};
use crate::core::validate::FieldError;

/// Validate that Select/Radio value exists in the options list.
pub(crate) fn check_option_valid(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&serde_json::Value>,
    is_empty: bool,
    errors: &mut Vec<FieldError>,
) {
    if (field.field_type != FieldType::Select && field.field_type != FieldType::Radio)
        || is_empty || field.options.is_empty()
    {
        return;
    }
    if let Some(serde_json::Value::String(s)) = value {
        if field.has_many {
            // has_many select: value is a JSON array string like '["val1","val2"]'
            if let Ok(values) = serde_json::from_str::<Vec<String>>(s) {
                for v in &values {
                    if !field.options.iter().any(|opt| opt.value == *v) {
                        errors.push(FieldError::with_key(data_key.to_owned(), format!("{} has an invalid option: {}", field.name, v), "validation.invalid_option_value", HashMap::from([("field".to_string(), field.name.clone()), ("value".to_string(), v.clone())])));
                        break;
                    }
                }
            }
        } else if !field.options.iter().any(|opt| opt.value == *s) {
            errors.push(FieldError::with_key(data_key.to_owned(), format!("{} has an invalid option", field.name), "validation.invalid_option", HashMap::from([("field".to_string(), field.name.clone())])));
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
    fn test_validate_select_option_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, color TEXT)").unwrap();
        let fields = vec![FieldDefinition::builder("color", FieldType::Select)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
                SelectOption::new(LocalizedString::Plain("Blue".to_string()), "blue"),
            ])
            .build()];
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
        let fields = vec![FieldDefinition::builder("color", FieldType::Select)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
            ])
            .build()];
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
        let fields = vec![FieldDefinition::builder("color", FieldType::Select)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
            ])
            .build()];
        let mut data = HashMap::new();
        data.insert("color".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Empty select value should pass (not required)");
    }

    #[test]
    fn test_validate_radio_option_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, size TEXT)").unwrap();
        let fields = vec![FieldDefinition::builder("size", FieldType::Radio)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Small".to_string()), "sm"),
                SelectOption::new(LocalizedString::Plain("Large".to_string()), "lg"),
            ])
            .build()];
        let mut data = HashMap::new();
        data.insert("size".to_string(), json!("sm"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Valid radio option should pass");
    }

    #[test]
    fn test_validate_radio_option_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, size TEXT)").unwrap();
        let fields = vec![FieldDefinition::builder("size", FieldType::Radio)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Small".to_string()), "sm"),
            ])
            .build()];
        let mut data = HashMap::new();
        data.insert("size".to_string(), json!("xl"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Invalid radio option should fail");
        assert!(result.unwrap_err().errors[0].message.contains("invalid option"));
    }

    #[test]
    fn test_validate_radio_option_empty_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, size TEXT)").unwrap();
        let fields = vec![FieldDefinition::builder("size", FieldType::Radio)
            .options(vec![
                SelectOption::new(LocalizedString::Plain("Small".to_string()), "sm"),
            ])
            .build()];
        let mut data = HashMap::new();
        data.insert("size".to_string(), json!(""));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Empty radio value should skip option validation");
    }

    #[test]
    fn test_validate_select_no_options_skips_option_check() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, status TEXT)").unwrap();
        let fields = vec![FieldDefinition::builder("status", FieldType::Select).build()];
        let mut data = HashMap::new();
        data.insert("status".to_string(), json!("anything"));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Select with no options should not validate option values");
    }
}
