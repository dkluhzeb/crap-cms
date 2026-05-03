use std::collections::HashMap;

use serde_json::Value;

use crate::core::{FieldDefinition, FieldType, validate::FieldError};

/// Validate individual values within a has_many JSON array.
/// Checks count bounds (min_rows/max_rows) for all has_many field types
/// and per-element constraints for Text/Number.
pub(crate) fn check_has_many_elements(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    is_empty: bool,
    errors: &mut Vec<FieldError>,
) {
    if !field.has_many || is_empty {
        return;
    }

    // Select/Radio: only check count bounds (per-element option validation is in check_option_valid)
    if field.field_type == FieldType::Select || field.field_type == FieldType::Radio {
        if let Some(Value::String(s)) = value
            && let Ok(values) = serde_json::from_str::<Vec<Value>>(s)
        {
            check_count_bounds(field, data_key, values.len(), errors);
        }

        return;
    }

    if field.field_type != FieldType::Text && field.field_type != FieldType::Number {
        return;
    }

    if let Some(Value::String(s)) = value
        && let Ok(values) = serde_json::from_str::<Vec<String>>(s)
    {
        check_count_bounds(field, data_key, values.len(), errors);

        for v in &values {
            match field.field_type {
                FieldType::Text => check_text_value_length(field, data_key, v, errors),
                FieldType::Number => check_number_value_bounds(field, data_key, v, errors),
                _ => {}
            }
        }
    }
}

/// Validate a single text value against min_length/max_length constraints.
fn check_text_value_length(
    field: &FieldDefinition,
    data_key: &str,
    v: &str,
    errors: &mut Vec<FieldError>,
) {
    let char_count = v.chars().count();

    if let Some(min_len) = field.min_length
        && char_count < min_len
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!(
                "{}: '{}' must be at least {} characters",
                field.name, v, min_len
            ),
            "validation.has_many_min_length",
            HashMap::from([
                ("field".to_string(), field.name.clone()),
                ("value".to_string(), v.to_string()),
                ("min".to_string(), min_len.to_string()),
            ]),
        ));
    }

    if let Some(max_len) = field.max_length
        && char_count > max_len
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!(
                "{}: '{}' must be at most {} characters",
                field.name, v, max_len
            ),
            "validation.has_many_max_length",
            HashMap::from([
                ("field".to_string(), field.name.clone()),
                ("value".to_string(), v.to_string()),
                ("max".to_string(), max_len.to_string()),
            ]),
        ));
    }
}

/// Validate a single number value against min/max constraints.
fn check_number_value_bounds(
    field: &FieldDefinition,
    data_key: &str,
    v: &str,
    errors: &mut Vec<FieldError>,
) {
    let Ok(num) = v.parse::<f64>() else {
        return;
    };

    if let Some(min_val) = field.min
        && num < min_val
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{}: {} must be at least {}", field.name, v, min_val),
            "validation.has_many_min_value",
            HashMap::from([
                ("field".to_string(), field.name.clone()),
                ("value".to_string(), v.to_string()),
                ("min".to_string(), min_val.to_string()),
            ]),
        ));
    }

    if let Some(max_val) = field.max
        && num > max_val
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{}: {} must be at most {}", field.name, v, max_val),
            "validation.has_many_max_value",
            HashMap::from([
                ("field".to_string(), field.name.clone()),
                ("value".to_string(), v.to_string()),
                ("max".to_string(), max_val.to_string()),
            ]),
        ));
    }
}

/// Shared min_rows/max_rows validation for all has_many field types.
fn check_count_bounds(
    field: &FieldDefinition,
    data_key: &str,
    count: usize,
    errors: &mut Vec<FieldError>,
) {
    if let Some(min_rows) = field.min_rows
        && count < min_rows
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{} must have at least {} values", field.name, min_rows),
            "validation.has_many_min_rows",
            HashMap::from([
                ("field".to_string(), field.name.clone()),
                ("min".to_string(), min_rows.to_string()),
            ]),
        ));
    }

    if let Some(max_rows) = field.max_rows
        && count > max_rows
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{} must have at most {} values", field.name, max_rows),
            "validation.has_many_max_rows",
            HashMap::from([
                ("field".to_string(), field.name.clone()),
                ("max".to_string(), max_rows.to_string()),
            ]),
        ));
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use crate::core::field::{FieldDefinition, FieldType, LocalizedString, SelectOption};
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_validate_has_many_select_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
                    SelectOption::new(LocalizedString::Plain("Blue".to_string()), "blue"),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["red","blue"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "Valid has_many select values should pass");
    }

    #[test]
    fn test_validate_has_many_select_invalid_option() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Red".to_string()),
                    "red",
                )])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["red","invalid"]"#));
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
                .contains("invalid option")
        );
    }

    #[test]
    fn test_validate_has_many_select_empty_array() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Red".to_string()),
                    "red",
                )])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!("[]"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Empty array for has_many select should pass"
        );
    }

    #[test]
    fn test_validate_has_many_text_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["rust","lua","python"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "Valid has_many text values should pass");
    }

    #[test]
    fn test_validate_has_many_text_min_length_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .min_length(3)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["rust","ab"]"#));
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
                .contains("at least 3 characters")
        );
    }

    #[test]
    fn test_validate_has_many_text_max_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .max_rows(2)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["a","b","c"]"#));
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
                .contains("at most 2 values")
        );
    }

    #[test]
    fn test_validate_has_many_number_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, scores TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("scores", FieldType::Number)
                .has_many(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("scores".to_string(), json!(r#"["10","20","30"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "Valid has_many number values should pass");
    }

    #[test]
    fn test_validate_has_many_number_max_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, scores TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("scores", FieldType::Number)
                .has_many(true)
                .max(50.0)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("scores".to_string(), json!(r#"["10","75"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at most 50"));
    }

    #[test]
    fn test_has_many_text_required_empty_array_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .required(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!("[]"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "Empty array should fail required check");
        assert!(result.unwrap_err().errors[0].message.contains("required"));
    }

    #[test]
    fn test_has_many_text_required_with_values_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .required(true)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["rust"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "Non-empty array should pass required check");
    }

    #[test]
    fn test_has_many_text_max_length_not_applied_to_json_string() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .max_length(10)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["abcdefgh","abcdefgh"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "max_length should check per-value, not JSON string length"
        );
    }

    #[test]
    fn test_validate_has_many_text_min_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .min_rows(3)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["a","b"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "has_many text with fewer items than min_rows should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("at least 3"));
    }

    #[test]
    fn test_validate_has_many_number_min_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, scores TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("scores", FieldType::Number)
                .has_many(true)
                .min_rows(2)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("scores".to_string(), json!(r#"["10"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "has_many number with fewer items than min_rows should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("at least 2"));
    }

    #[test]
    fn test_validate_has_many_number_min_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, scores TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("scores", FieldType::Number)
                .has_many(true)
                .min(5.0)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("scores".to_string(), json!(r#"["10","2"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "has_many number with value below min should fail"
        );
        assert!(result.unwrap_err().errors[0].message.contains("at least 5"));
    }

    #[test]
    fn test_validate_has_many_text_max_length_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .max_length(3)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["ab","toolong"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "has_many text with value exceeding max_length should fail"
        );
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("at most 3 characters")
        );
    }

    /// Regression: has_many validation must report ALL invalid values, not just the first.
    /// Previously, `break` after the first error caused subsequent violations to be hidden.
    #[test]
    fn test_has_many_reports_all_invalid_values() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();

        // Three values all below min_length=5
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .min_length(5)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["ab","cd","ef"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        let errors = &result.unwrap_err().errors;
        assert_eq!(
            errors.len(),
            3,
            "All three invalid values should produce errors, got {}",
            errors.len()
        );
    }

    /// Regression: has_many number validation must report ALL out-of-bounds values.
    #[test]
    fn test_has_many_number_reports_all_invalid_values() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, scores TEXT)")
            .unwrap();

        let fields = vec![
            FieldDefinition::builder("scores", FieldType::Number)
                .has_many(true)
                .max(10.0)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("scores".to_string(), json!(r#"["20","30"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        let errors = &result.unwrap_err().errors;
        assert_eq!(
            errors.len(),
            2,
            "Both out-of-range values should produce errors, got {}",
            errors.len()
        );
    }

    /// Regression: has_many length validation must count characters, not bytes.
    /// Multibyte UTF-8 characters (emoji, CJK, accented) were overcounted with `.len()`.
    #[test]
    fn test_has_many_text_length_counts_chars_not_bytes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();

        // "café" = 4 chars but 5 bytes (é is 2 bytes in UTF-8)
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .max_length(4)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["café"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "café is 4 chars — should pass max_length=4");

        // "你好" = 2 chars but 6 bytes
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .min_length(2)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["你好"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "你好 is 2 chars — should pass min_length=2");
    }

    /// Regression: has_many Select must enforce min_rows/max_rows bounds.
    /// Previously, check_has_many_elements only handled Text/Number, so
    /// Select/Radio has_many fields silently bypassed row count validation.
    #[test]
    fn test_has_many_select_min_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .min_rows(2)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("A".to_string()), "a"),
                    SelectOption::new(LocalizedString::Plain("B".to_string()), "b"),
                    SelectOption::new(LocalizedString::Plain("C".to_string()), "c"),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["a"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "has_many select with 1 value should fail min_rows=2"
        );
        assert!(result.unwrap_err().errors[0].message.contains("at least 2"));
    }

    /// Regression: has_many Select must enforce max_rows bounds.
    #[test]
    fn test_has_many_select_max_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .max_rows(2)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("A".to_string()), "a"),
                    SelectOption::new(LocalizedString::Plain("B".to_string()), "b"),
                    SelectOption::new(LocalizedString::Plain("C".to_string()), "c"),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("tags".to_string(), json!(r#"["a","b","c"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "has_many select with 3 values should fail max_rows=2"
        );
        assert!(result.unwrap_err().errors[0].message.contains("at most 2"));
    }

    /// Regression: has_many Radio must enforce min_rows bounds.
    #[test]
    fn test_has_many_radio_min_rows_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, sizes TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("sizes", FieldType::Radio)
                .has_many(true)
                .min_rows(2)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("S".to_string()), "s"),
                    SelectOption::new(LocalizedString::Plain("M".to_string()), "m"),
                    SelectOption::new(LocalizedString::Plain("L".to_string()), "l"),
                ])
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("sizes".to_string(), json!(r#"["s"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "has_many radio with 1 value should fail min_rows=2"
        );
        assert!(result.unwrap_err().errors[0].message.contains("at least 2"));
    }
}
