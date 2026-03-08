use crate::core::field::{FieldDefinition, FieldType};
use crate::core::validate::FieldError;

/// Check required constraint. Skipped for checkboxes, drafts, and partial updates.
/// For Array and has-many Relationship, "required" means at least one item.
pub(crate) fn check_required(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&serde_json::Value>,
    is_empty: bool,
    is_draft: bool,
    is_update: bool,
    errors: &mut Vec<FieldError>,
) {
    if !field.required || is_draft || field.field_type == FieldType::Checkbox
        || (is_update && value.is_none())
    {
        return;
    }

    if !field.has_parent_column() {
        let has_items = match value {
            Some(serde_json::Value::Array(arr)) => !arr.is_empty(),
            Some(serde_json::Value::String(s)) => !s.is_empty(),
            _ => false,
        };
        if !has_items {
            errors.push(FieldError::new(data_key.to_owned(), format!("{} is required", field.name)));
        }
    } else if field.has_many {
        let has_items = match value {
            Some(serde_json::Value::String(s)) => {
                serde_json::from_str::<Vec<serde_json::Value>>(s)
                    .map(|arr| !arr.is_empty())
                    .unwrap_or(!s.is_empty())
            }
            _ => false,
        };
        if !has_items {
            errors.push(FieldError::new(data_key.to_owned(), format!("{} is required", field.name)));
        }
    } else if is_empty {
        errors.push(FieldError::new(data_key.to_owned(), format!("{} is required", field.name)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::lifecycle::validation::validate_fields_inner;
    use serde_json::json;
    use std::collections::HashMap;

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
            relationship: Some(crate::core::field::RelationshipConfig::new("tags", true)),
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
            relationship: Some(crate::core::field::RelationshipConfig::new("tags", true)),
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(["t1", "t2"]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
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
        let data = HashMap::new();
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_required_array_field_empty_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            required: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_err(), "Empty array for required array field should fail");
        assert!(result.unwrap_err().errors[0].message.contains("required"));
    }

    #[test]
    fn test_validate_required_array_field_non_empty_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)").unwrap();
        let fields = vec![FieldDefinition {
            name: "items".to_string(),
            field_type: FieldType::Array,
            required: true,
            ..Default::default()
        }];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"x": 1}]));
        let result = validate_fields_inner(&lua, &fields, &data, &conn, "test", None, false);
        assert!(result.is_ok(), "Non-empty array for required array field should pass");
    }
}
