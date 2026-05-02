use std::collections::HashMap;

use serde_json::Value;

use crate::core::{FieldDefinition, validate::FieldError};

/// Validate min / max bounds for number fields.
/// Skipped for has_many fields (validated per-element in `check_has_many_elements`).
pub(crate) fn check_numeric_bounds(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    is_empty: bool,
    errors: &mut Vec<FieldError>,
) {
    if is_empty || field.has_many || (field.min.is_none() && field.max.is_none()) {
        return;
    }

    let num_val = match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.parse::<f64>().ok(),
        _ => None,
    };

    let Some(v) = num_val else {
        return;
    };

    // NaN and ±Infinity slip past min/max comparisons (every NaN
    // comparison returns false, ∞ trivially passes any finite max),
    // so a user submitting `"NaN"` or `"Infinity"` for a number field
    // would otherwise reach the DB unchallenged and break downstream
    // filters / aggregations (NaN ≠ NaN, no row ever matches).
    if !v.is_finite() {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{} must be a finite number", field.name),
            "validation.finite_number",
            HashMap::from([("field".to_string(), field.name.clone())]),
        ));
        return;
    }

    if let Some(min_val) = field.min
        && v < min_val
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{} must be at least {}", field.name, min_val),
            "validation.min_value",
            HashMap::from([
                ("field".to_string(), field.name.clone()),
                ("min".to_string(), min_val.to_string()),
            ]),
        ));
    }

    if let Some(max_val) = field.max
        && v > max_val
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{} must be at most {}", field.name, max_val),
            "validation.max_value",
            HashMap::from([
                ("field".to_string(), field.name.clone()),
                ("max".to_string(), max_val.to_string()),
            ]),
        ));
    }
}

#[cfg(test)]
mod tests {
    use crate::core::field::{FieldDefinition, FieldType};
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn test_validate_number_min_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, score REAL)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("score", FieldType::Number)
                .min(0.0)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("score".to_string(), json!("-5"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at least 0"));
    }

    #[test]
    fn test_validate_number_max_fails() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, score REAL)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("score", FieldType::Number)
                .max(100.0)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("score".to_string(), json!("150"));
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
                .contains("at most 100")
        );
    }

    #[test]
    fn test_validate_number_min_max_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, score REAL)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("score", FieldType::Number)
                .min(0.0)
                .max(100.0)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("score".to_string(), json!("50"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_number_min_max_skipped_for_empty() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, score REAL)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("score", FieldType::Number)
                .min(10.0)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("score".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "min/max should not trigger on empty values");
    }

    #[test]
    fn test_validate_number_json_number_value() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, score REAL)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("score", FieldType::Number)
                .min(0.0)
                .max(10.0)
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("score".to_string(), json!(15));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("at most 10"));
    }

    /// Regression: `"NaN"` parses to `f64::NAN`, which slips past
    /// `< min` / `> max` checks (NaN comparisons all return false).
    /// Without an explicit finite-check the value is silently accepted
    /// and persisted, then breaks downstream filters and aggregations
    /// (NaN ≠ NaN, no row ever matches). Same applies to `"Infinity"`
    /// and `"-Infinity"`.
    #[test]
    fn nan_and_infinity_inputs_are_rejected() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, score REAL)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("score", FieldType::Number)
                .min(0.0)
                .max(100.0)
                .build(),
        ];

        for bad in ["NaN", "Infinity", "-Infinity", "inf", "-inf"] {
            let mut data = HashMap::new();
            data.insert("score".to_string(), json!(bad));
            let result = validate_fields_inner(
                &lua,
                &fields,
                &data,
                &ValidationCtx::builder(&conn, "test").build(),
            );
            assert!(
                result.is_err(),
                "input {bad:?} must be rejected as non-finite",
            );
            let msg = &result.unwrap_err().errors[0].message;
            assert!(
                msg.contains("finite"),
                "expected finite-number error for {bad:?}, got: {msg}",
            );
        }
    }

    /// Sanity: ordinary finite values still pass.
    #[test]
    fn finite_values_pass_with_bounds() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, score REAL)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("score", FieldType::Number)
                .min(0.0)
                .max(100.0)
                .build(),
        ];

        for ok in ["0", "50.5", "100", "1e2"] {
            let mut data = HashMap::new();
            data.insert("score".to_string(), json!(ok));
            let result = validate_fields_inner(
                &lua,
                &fields,
                &data,
                &ValidationCtx::builder(&conn, "test").build(),
            );
            assert!(result.is_ok(), "input {ok:?} should pass validation");
        }
    }
}
