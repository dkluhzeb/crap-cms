use std::collections::HashMap;

use mlua::Lua;
use serde_json::Value;
use tracing::warn;

use crate::{
    core::{FieldDefinition, validate::FieldError},
    hooks::lifecycle::validation::custom::run_validate_function_inner,
};

/// Run custom Lua validate function on a field value.
pub(crate) fn check_custom_validate(
    lua: &Lua,
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    data: &HashMap<String, Value>,
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
            warn!("Validate function '{}' error: {}", validate_ref, e);

            errors.push(FieldError::with_key(
                data_key.to_owned(),
                format!("Validation failed (internal error in '{}')", validate_ref),
                "validation.custom_error",
                HashMap::from([("field".to_string(), field.name.clone())]),
            ));
        }
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use crate::core::field::{FieldDefinition, FieldType};
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_validate_custom_validate_function_returns_error() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = {
                validate_test = function(value, ctx)

                    if value == "bad" then

                        return "value cannot be bad"
                    end

                    return true
                end
            }
        "#,
        )
        .exec()
        .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .validate("validators.validate_test")
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("bad"));
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
                .contains("cannot be bad")
        );
    }

    #[test]
    fn test_validate_custom_validate_function_returns_false() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_fail = function(value, ctx)

                return false
            end
        "#,
        )
        .exec()
        .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .validate("validators.validate_fail")
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("anything"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().errors[0].message, "validation failed");
    }

    #[test]
    fn test_validate_custom_validate_function_returns_true() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_ok = function(value, ctx)

                return true
            end
        "#,
        )
        .exec()
        .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .validate("validators.validate_ok")
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("good"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok());
    }

    /// Regression: when a custom validate function errors (Lua runtime error),
    /// validation must fail — not silently pass. Swallowing the error could
    /// allow invalid data to be persisted.
    #[test]
    fn test_validate_custom_function_error_fails_validation() {
        let lua = mlua::Lua::new();
        lua.load(
            r#"
            package.loaded["validators"] = package.loaded["validators"] or {}
            package.loaded["validators"].validate_boom = function(value, ctx)
                error("unexpected error in validator")
            end
        "#,
        )
        .exec()
        .unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, name TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("name", FieldType::Text)
                .validate("validators.validate_boom")
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("name".to_string(), json!("anything"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Lua error in custom validator must fail validation"
        );
        let errs = result.unwrap_err().errors;
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].key.as_deref(), Some("validation.custom_error"));
    }
}
