use serde_json::Value;

use crate::core::{FieldDefinition, FieldType, validate::FieldError};

use super::shared::{decode_element_list, element_display};

/// Validate that Select/Radio value exists in the options list. A present
/// value of the wrong shape (non-string single value, undecodable `has_many`
/// list) is an invalid option — never a silent pass.
pub(crate) fn check_option_valid(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    is_empty: bool,
    errors: &mut Vec<FieldError>,
) {
    if (field.field_type != FieldType::Select && field.field_type != FieldType::Radio)
        || is_empty
        || field.options.is_empty()
    {
        return;
    }

    if field.has_many {
        check_has_many_options(field, data_key, value, errors);
        return;
    }

    let valid = matches!(
        value,
        Some(Value::String(s)) if field.options.iter().any(|opt| opt.value == *s)
    );
    if !valid {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{} has an invalid option", field.name),
                "validation.invalid_option",
            )
            .with_param("field", field.name.clone()),
        );
    }
}

/// Validate each value in a `has_many` select/radio element list against the
/// options list. Accepts both the typed array and JSON-string encodings.
fn check_has_many_options(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    errors: &mut Vec<FieldError>,
) {
    let Some(values) = decode_element_list(value) else {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!(
                    "{} has invalid multi-select value (malformed JSON)",
                    field.name
                ),
                "validation.invalid_multi_select_json",
            )
            .with_param("field", field.name.clone()),
        );

        return;
    };

    for v in &values {
        let valid = v
            .as_str()
            .is_some_and(|s| field.options.iter().any(|opt| opt.value == s));

        if !valid {
            errors.push(
                FieldError::with_key(
                    data_key.to_owned(),
                    format!(
                        "{} has an invalid option: {}",
                        field.name,
                        element_display(v)
                    ),
                    "validation.invalid_option_value",
                )
                .with_param("field", field.name.clone())
                .with_param("value", element_display(v)),
            );
        }
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use crate::core::DocumentFields;
    use crate::core::{FieldDefinition, FieldType, LocalizedString, SelectOption};
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;

    /// Regression: a `has_many` select submitted as a typed array (Lua/gRPC)
    /// bypassed the options allowlist entirely — only the JSON-string
    /// encoding was checked.
    #[test]
    fn has_many_select_typed_array_invalid_option_rejected() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, colors TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("colors", FieldType::Select)
                .has_many(true)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
                    SelectOption::new(LocalizedString::Plain("Blue".to_string()), "blue"),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("colors".to_string(), json!(["red", "green"]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "'green' is not an allowed option");
        let err = result.unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.invalid_option_value")),
            "expected invalid_option_value, got: {:?}",
            err.errors
        );
    }

    /// Regression: a present non-string value on a single Select/Radio
    /// silently bypassed the allowlist (e.g. `status = 5` persisted).
    #[test]
    fn single_select_non_string_value_rejected() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, color TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("color", FieldType::Select)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Red".to_string()),
                    "red",
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("color".to_string(), json!(5));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "non-string select value must be rejected");
        let err = result.unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.invalid_option")),
            "expected invalid_option, got: {:?}",
            err.errors
        );
    }

    #[test]
    fn test_validate_select_option_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, color TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("color", FieldType::Select)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Red".to_string()), "red"),
                    SelectOption::new(LocalizedString::Plain("Blue".to_string()), "blue"),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("color".to_string(), json!("red"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_select_option_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, color TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("color", FieldType::Select)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Red".to_string()),
                    "red",
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("color".to_string(), json!("green"));
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
    fn test_validate_select_option_empty_value_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, color TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("color", FieldType::Select)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Red".to_string()),
                    "red",
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("color".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Empty select value should pass (not required)"
        );
    }

    #[test]
    fn test_validate_radio_option_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, size TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("size", FieldType::Radio)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("Small".to_string()), "sm"),
                    SelectOption::new(LocalizedString::Plain("Large".to_string()), "lg"),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("size".to_string(), json!("sm"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "Valid radio option should pass");
    }

    #[test]
    fn test_validate_radio_option_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, size TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("size", FieldType::Radio)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Small".to_string()),
                    "sm",
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("size".to_string(), json!("xl"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "Invalid radio option should fail");
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("invalid option")
        );
    }

    #[test]
    fn test_validate_radio_option_empty_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, size TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("size", FieldType::Radio)
                .options(vec![SelectOption::new(
                    LocalizedString::Plain("Small".to_string()),
                    "sm",
                )])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("size".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Empty radio value should skip option validation"
        );
    }

    /// Regression: malformed JSON in a `has_many` select must produce a
    /// validation error, not silently pass.
    #[test]
    fn test_validate_has_many_select_malformed_json_rejected() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Select)
                .has_many(true)
                .options(vec![
                    SelectOption::new(LocalizedString::Plain("A".to_string()), "a"),
                    SelectOption::new(LocalizedString::Plain("B".to_string()), "b"),
                ])
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!("[invalid json"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "Malformed has_many JSON must be rejected");
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("malformed JSON"),
        );
    }

    /// Regression: `has_many` select must report ALL invalid options, not just the first.
    /// Previously, `break` after the first error caused subsequent violations to be hidden.
    #[test]
    fn test_has_many_select_reports_all_invalid_options() {
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
        let mut data = DocumentFields::new();
        // Two invalid options: "invalid1" and "invalid2"
        data.insert(
            "tags".to_string(),
            json!(r#"["invalid1","red","invalid2"]"#),
        );
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
            "Both invalid options should produce errors, got {}",
            errors.len()
        );
    }

    #[test]
    fn test_validate_select_no_options_skips_option_check() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, status TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("status", FieldType::Select).build()];
        let mut data = DocumentFields::new();
        data.insert("status".to_string(), json!("anything"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Select with no options should not validate option values"
        );
    }
}
