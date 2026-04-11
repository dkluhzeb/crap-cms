use serde_json::Value;

use crate::core::{FieldDefinition, FieldType, validate::FieldError};
use std::collections::HashMap;

/// Check required constraint. Skipped for checkboxes, drafts, and partial updates.
/// For Array and has-many Relationship, "required" means at least one item.
pub(crate) fn check_required(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    is_empty: bool,
    is_draft: bool,
    is_update: bool,
    errors: &mut Vec<FieldError>,
) {
    if !field.required
        || is_draft
        || field.field_type == FieldType::Checkbox
        || (is_update && value.is_none())
    {
        return;
    }

    if is_value_present(field, value, is_empty) {
        return;
    }

    errors.push(FieldError::with_key(
        data_key.to_owned(),
        format!("{} is required", field.name),
        "validation.required",
        HashMap::from([("field".to_string(), field.name.clone())]),
    ));
}

/// Check if a field value is "present" for required validation purposes.
fn is_value_present(field: &FieldDefinition, value: Option<&Value>, is_empty: bool) -> bool {
    if !field.has_parent_column() {
        // Array/Blocks/Relationship: check for non-empty array or string
        return match value {
            Some(Value::Array(arr)) => !arr.is_empty(),
            Some(Value::String(s)) => !s.is_empty(),
            _ => false,
        };
    }

    if field.has_many {
        // has_many with parent column: value is a JSON array string
        return match value {
            Some(Value::String(s)) => serde_json::from_str::<Vec<Value>>(s)
                .map(|arr| !arr.is_empty())
                .unwrap_or(!s.is_empty()),
            _ => false,
        };
    }

    !is_empty
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::field::RelationshipConfig;
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_validate_required_field_empty_string() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .required(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.errors.len(), 1);
        assert!(err.errors[0].message.contains("required"));
    }

    #[test]
    fn test_validate_required_field_null() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .required(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!(null));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_required_skipped_for_drafts() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .required(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").draft(true).build(),
        );
        assert!(result.is_ok(), "Drafts should skip required checks");
    }

    #[test]
    fn test_validate_required_join_field_empty_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .required(true)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!([]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("required"));
    }

    #[test]
    fn test_validate_required_join_field_non_empty_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .required(true)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(["t1", "t2"]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_required_skipped_on_update_absent_field() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .required(true)
                .build(),
        ];
        let data = HashMap::new();
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test")
                .exclude_id(Some("p1"))
                .build(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_checkbox_not_required() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, active INTEGER)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("active", FieldType::Checkbox)
                .required(true)
                .build(),
        ];
        let data = HashMap::new();
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_required_array_field_empty_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .required(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Empty array for required array field should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("required"));
    }

    #[test]
    fn test_validate_required_array_field_non_empty_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("items", FieldType::Array)
                .required(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("items".to_string(), json!([{"x": 1}]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Non-empty array for required array field should pass"
        );
    }
}
