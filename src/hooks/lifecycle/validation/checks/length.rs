use std::collections::HashMap;

use serde_json::Value;

use crate::core::{FieldDefinition, validate::FieldError};

/// Validate min_length / max_length for text/textarea fields.
/// Skipped for has_many fields (validated per-element in `check_has_many_elements`).
pub(crate) fn check_length_bounds(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    is_empty: bool,
    errors: &mut Vec<FieldError>,
) {
    if is_empty || field.has_many || (field.min_length.is_none() && field.max_length.is_none()) {
        return;
    }

    let Some(Value::String(s)) = value else {
        return;
    };

    let len = s.chars().count();

    if let Some(min_len) = field.min_length
        && len < min_len
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{} must be at least {} characters", field.name, min_len),
            "validation.min_length",
            HashMap::from([
                ("field".to_string(), field.name.clone()),
                ("min".to_string(), min_len.to_string()),
            ]),
        ));
    }

    if let Some(max_len) = field.max_length
        && len > max_len
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{} must be at most {} characters", field.name, max_len),
            "validation.max_length",
            HashMap::from([
                ("field".to_string(), field.name.clone()),
                ("max".to_string(), max_len.to_string()),
            ]),
        ));
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use crate::core::field::{FieldDefinition, FieldType};
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_validate_min_length_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .min_length(5)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("ab"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("at least 5 characters")
        );
    }

    #[test]
    fn test_validate_min_length_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .min_length(3)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("hello"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_max_length_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .max_length(5)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("toolongvalue"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("at most 5 characters")
        );
    }

    #[test]
    fn test_validate_max_length_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .max_length(10)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("short"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok());
    }

    /// Regression: length validation must count characters, not bytes.
    /// Multibyte UTF-8 characters (emoji, CJK, accented) were overcounted.
    #[test]
    fn test_validate_length_counts_chars_not_bytes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();

        // "café" = 4 chars but 5 bytes (é is 2 bytes)
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .max_length(4)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("café"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "café is 4 chars — should pass max_length=4");

        // "你好世界" = 4 chars but 12 bytes
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .min_length(4)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("你好世界"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "你好世界 is 4 chars — should pass min_length=4"
        );
    }

    #[test]
    fn test_validate_min_max_length_skipped_for_empty() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .min_length(5)
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
        assert!(
            result.is_ok(),
            "min_length should not trigger on empty values"
        );
    }
}
