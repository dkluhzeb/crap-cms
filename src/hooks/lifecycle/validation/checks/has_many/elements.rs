//! Per-element checks of a `has_many` Text or Number list: each text element
//! must be text within the length bounds, each number element a finite
//! (whole, when `integer`) number within `min`/`max`.

use serde_json::Value;

use crate::{
    core::{FieldDefinition, validate::FieldError},
    db::query::helpers::number_element,
    hooks::lifecycle::validation::checks::{
        length::not_text_error,
        numeric::{NumberViolation, number_violation},
        shared::element_display,
    },
};

/// Validate one element of a text list: it must be text — the write would
/// store a number or a list as its JSON spelling — and within the length
/// bounds.
pub(super) fn check_text_element(
    field: &FieldDefinition,
    data_key: &str,
    element: &Value,
    errors: &mut Vec<FieldError>,
) {
    let Some(s) = element.as_str() else {
        errors.push(not_text_error(field, data_key));

        return;
    };

    check_text_value_length(field, data_key, s, errors);
}

/// Validate a single text value against `min_length/max_length` constraints.
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
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!(
                    "{}: '{}' must be at least {} characters",
                    field.name, v, min_len
                ),
                "validation.has_many_min_length",
            )
            .with_param("field", field.name.clone())
            .with_param("value", v.to_string())
            .with_param("min", min_len.to_string()),
        );
    }

    if let Some(max_len) = field.max_length
        && char_count > max_len
    {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!(
                    "{}: '{}' must be at most {} characters",
                    field.name, v, max_len
                ),
                "validation.has_many_max_length",
            )
            .with_param("field", field.name.clone())
            .with_param("value", v.to_string())
            .with_param("max", max_len.to_string()),
        );
    }
}

/// Validate a single number value against min/max constraints. Elements
/// arrive as JSON numbers (typed surfaces) or number-strings (admin form).
pub(super) fn check_number_value_bounds(
    field: &FieldDefinition,
    data_key: &str,
    element: &Value,
    errors: &mut Vec<FieldError>,
) {
    let num = number_element(element);
    let v = element_display(element);
    let v = v.as_str();

    // A non-numeric element would be silently dropped by the write-edge
    // coercion (data loss) — reject it, matching the single-value path.
    let Some(num) = num else {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{}: {} must be a number", field.name, v),
                "validation.has_many_invalid_number",
            )
            .with_param("field", field.name.clone())
            .with_param("value", v.to_string()),
        );
        return;
    };

    // Finite + integer rule, shared with the single-value numeric path.
    if let Some(violation) = number_violation(field, num) {
        let (message, key) = match violation {
            NumberViolation::NotFinite => (
                format!("{}: {} must be a finite number", field.name, v),
                "validation.has_many_finite_number",
            ),
            NumberViolation::NotWhole => (
                format!("{}: {} must be a whole number", field.name, v),
                "validation.has_many_whole_number",
            ),
        };
        errors.push(
            FieldError::with_key(data_key.to_owned(), message, key)
                .with_param("field", field.name.clone())
                .with_param("value", v.to_string()),
        );
        return;
    }

    if let Some(min_val) = field.min
        && num < min_val
    {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{}: {} must be at least {}", field.name, v, min_val),
                "validation.has_many_min_value",
            )
            .with_param("field", field.name.clone())
            .with_param("value", v.to_string())
            .with_param("min", min_val.to_string()),
        );
    }

    if let Some(max_val) = field.max
        && num > max_val
    {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{}: {} must be at most {}", field.name, v, max_val),
                "validation.has_many_max_value",
            )
            .with_param("field", field.name.clone())
            .with_param("value", v.to_string())
            .with_param("max", max_val.to_string()),
        );
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use serde_json::{Value, json};

    use crate::{
        core::{DocumentFields, FieldDefinition, FieldType},
        hooks::lifecycle::validation::{
            ValidationCtx,
            checks::{HasManyCheck, check_has_many_elements},
            validate_fields_inner,
        },
    };

    /// Regression: validation parsed a number element untrimmed while the write
    /// trims it, so `" 5"` was rejected though the write would store `5`.
    #[test]
    fn a_padded_number_element_is_valid() {
        let field = FieldDefinition::builder("scores", FieldType::Number)
            .has_many(true)
            .build();
        let mut errors = Vec::new();

        check_has_many_elements(
            &HasManyCheck::new(&field, "scores", Some(&json!([" 5"])), false),
            &mut errors,
        );

        assert!(errors.is_empty(), "{errors:?}");
    }

    /// Regression: a non-string element of a text list (`[1, ["a"]]`) passed
    /// validation and was stored as its JSON spelling. Every element must be
    /// text.
    #[test]
    fn a_non_string_element_of_a_text_list_is_rejected() {
        let field = FieldDefinition::builder("tags", FieldType::Text)
            .has_many(true)
            .build();
        let mut errors = Vec::new();

        check_has_many_elements(
            &HasManyCheck::new(&field, "tags", Some(&json!(["ok", 1, ["a"]])), false),
            &mut errors,
        );

        let keys: Vec<&str> = errors.iter().filter_map(|e| e.key.as_deref()).collect();
        assert_eq!(keys, vec!["validation.invalid_text"; 2]);
    }

    /// Regression: elements submitted as a typed array (Lua/gRPC) were
    /// silently skipped — the check only understood the JSON-string
    /// encoding, so per-element bounds and counts never ran.
    #[test]
    fn typed_array_elements_validated() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, tags TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Text)
                .has_many(true)
                .min_length(2)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!(["ab", "x"]));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "element 'x' violates min_length=2");
        let err = result.unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.has_many_min_length")),
            "expected has_many_min_length, got: {:?}",
            err.errors
        );
    }

    fn number_list_error_keys(scores: Value, integer: bool) -> Vec<String> {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, scores TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("scores", FieldType::Number)
                .has_many(true)
                .integer(integer)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("scores".to_string(), scores);
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        result
            .err()
            .map(|e| e.errors.iter().filter_map(|fe| fe.key.clone()).collect())
            .unwrap_or_default()
    }

    /// Regression: `has_many` Number elements skipped the finite/NaN,
    /// `integer`, and non-numeric hardening the single-value path enforces
    /// (only min/max ran), so `["NaN"]` / `["1.5"]` on an integer field /
    /// `["abc"]` slipped through — the last one then silently dropped on write.
    #[test]
    fn has_many_number_rejects_non_finite() {
        assert!(
            number_list_error_keys(json!(["NaN"]), false)
                .contains(&"validation.has_many_finite_number".to_string())
        );
    }

    #[test]
    fn has_many_number_rejects_fractional_when_integer() {
        assert!(
            number_list_error_keys(json!(["1.5"]), true)
                .contains(&"validation.has_many_whole_number".to_string())
        );
    }

    #[test]
    fn has_many_number_rejects_non_numeric() {
        assert!(
            number_list_error_keys(json!(["abc"]), false)
                .contains(&"validation.has_many_invalid_number".to_string())
        );
    }

    #[test]
    fn has_many_number_valid_list_passes() {
        assert!(number_list_error_keys(json!([1, 2, 3]), true).is_empty());
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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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

    /// Regression: `has_many` validation must report ALL invalid values, not just the first.
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
        let mut data = DocumentFields::new();
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

    /// Regression: `has_many` number validation must report ALL out-of-bounds values.
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
        let mut data = DocumentFields::new();
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

    /// Regression: `has_many` length validation must count characters, not bytes.
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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
        data.insert("tags".to_string(), json!(r#"["你好"]"#));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "你好 is 2 chars — should pass min_length=2");
    }
}
