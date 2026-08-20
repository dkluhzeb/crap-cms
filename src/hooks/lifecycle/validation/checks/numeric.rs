use serde_json::Value;

use crate::core::{FieldDefinition, FieldType, validate::FieldError};
use crate::db::f64_to_exact_i64;

/// Why a numeric value is unacceptable for `field`, independent of min/max.
/// Shared by the single-value ([`check_numeric_bounds`]) and per-element
/// (`has_many`, in `checks::has_many`) paths so both enforce the same rule.
pub(crate) enum NumberViolation {
    /// NaN or ±Infinity — slips past every min/max comparison and breaks
    /// downstream filters/aggregations (`NaN != NaN`).
    NotFinite,
    /// A fractional value for an `integer = true` field.
    NotWhole,
}

/// The [`NumberViolation`] for `v` under `field`, or `None` if it is acceptable
/// (bounds are checked separately). One rule, two call sites.
pub(crate) fn number_violation(field: &FieldDefinition, v: f64) -> Option<NumberViolation> {
    if !v.is_finite() {
        return Some(NumberViolation::NotFinite);
    }

    if field.integer && f64_to_exact_i64(v).is_none() {
        return Some(NumberViolation::NotWhole);
    }

    None
}

/// Validate min / max bounds for number fields, plus whole-number rejection
/// for `Integer` fields and numeric-type rejection for Number fields.
/// Skipped for `has_many` fields (validated per-element in
/// `check_has_many_elements`).
pub(crate) fn check_numeric_bounds(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    is_empty: bool,
    errors: &mut Vec<FieldError>,
) {
    // `integer = true` on a number field forces whole-number validation even
    // when no min/max bounds are set.
    let is_integer = field.integer;

    if is_empty || field.has_many {
        return;
    }

    let is_number_field = field.field_type == FieldType::Number;

    if !is_number_field && !is_integer && field.min.is_none() && field.max.is_none() {
        return;
    }

    let num_val = match value {
        Some(Value::Number(n)) => n.as_f64(),
        Some(Value::String(s)) => s.parse::<f64>().ok(),
        _ => None,
    };

    let Some(v) = num_val else {
        // A present, non-empty value that isn't numeric at all: the persist
        // edge would coerce it to NULL — silent data loss, and a `required`
        // bypass (the value counted as present). Reject it instead.
        if is_number_field {
            errors.push(
                FieldError::with_key(
                    data_key.to_owned(),
                    format!("{} must be a number", field.name),
                    "validation.invalid_number",
                )
                .with_param("field", field.name.clone()),
            );
        }
        return;
    };

    // Finite + integer rule, shared with the per-element `has_many` path.
    if let Some(violation) = number_violation(field, v) {
        let (message, key) = match violation {
            NumberViolation::NotFinite => (
                format!("{} must be a finite number", field.name),
                "validation.finite_number",
            ),
            NumberViolation::NotWhole => (
                format!("{} must be a whole number", field.name),
                "validation.whole_number",
            ),
        };
        errors.push(
            FieldError::with_key(data_key.to_owned(), message, key)
                .with_param("field", field.name.clone()),
        );
        return;
    }

    if let Some(min_val) = field.min
        && v < min_val
    {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{} must be at least {}", field.name, min_val),
                "validation.min_value",
            )
            .with_param("field", field.name.clone())
            .with_param("min", min_val.to_string()),
        );
    }

    if let Some(max_val) = field.max
        && v > max_val
    {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{} must be at most {}", field.name, max_val),
                "validation.max_value",
            )
            .with_param("field", field.name.clone())
            .with_param("max", max_val.to_string()),
        );
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use crate::core::DocumentFields;
    use crate::core::{FieldDefinition, FieldType};
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;

    /// Regression: a non-numeric value on a Number field passed all
    /// validation (parse failure was a silent early-return) and the persist
    /// edge coerced it to NULL — silent data loss that also bypassed
    /// `required` and every bound.
    #[test]
    fn non_numeric_value_on_number_field_rejected() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, score REAL)")
            .unwrap();

        // With bounds set (the original repro)…
        let bounded = vec![
            FieldDefinition::builder("score", FieldType::Number)
                .min(0.0)
                .build(),
        ];
        let mut data = DocumentFields::new();
        data.insert("score".to_string(), json!("abc"));
        let result = validate_fields_inner(
            &lua,
            &bounded,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err(), "'abc' must be rejected on a bounded field");

        // …and with no constraints at all (would still persist NULL).
        let unbounded = vec![FieldDefinition::builder("score", FieldType::Number).build()];
        let mut data = DocumentFields::new();
        data.insert("score".to_string(), json!("abc"));
        let result = validate_fields_inner(
            &lua,
            &unbounded,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "'abc' must be rejected even without bounds"
        );
        let err = result.unwrap_err();
        assert!(
            err.errors
                .iter()
                .any(|e| e.key.as_deref() == Some("validation.invalid_number")),
            "expected invalid_number, got: {:?}",
            err.errors
        );

        // Numeric strings stay accepted (admin form encoding).
        let mut data = DocumentFields::new();
        data.insert("score".to_string(), json!("42.5"));
        let result = validate_fields_inner(
            &lua,
            &unbounded,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "numeric strings must still pass: {result:?}"
        );
    }

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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
            let mut data = DocumentFields::new();
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

    /// A `number` field with `integer = true` rejects fractional input but
    /// accepts whole numbers (including whole-valued floats like `42.0`).
    #[test]
    fn integer_flag_rejects_fractions() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, qty REAL)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("qty", FieldType::Number)
                .integer(true)
                .build(),
        ];

        // Fractional → rejected.
        for bad in [json!(42.5), json!("42.5"), json!("3.14")] {
            let mut data = DocumentFields::new();
            data.insert("qty".to_string(), bad.clone());
            let result = validate_fields_inner(
                &lua,
                &fields,
                &data,
                &ValidationCtx::builder(&conn, "test").build(),
            );
            assert!(result.is_err(), "input {bad:?} must be rejected");
            assert!(
                result.unwrap_err().errors[0]
                    .message
                    .contains("whole number"),
                "expected whole-number error for {bad:?}",
            );
        }

        // Whole values (int, whole float, whole string) → accepted.
        for ok in [json!(42), json!(42.0), json!("42"), json!(-7)] {
            let mut data = DocumentFields::new();
            data.insert("qty".to_string(), ok.clone());
            let result = validate_fields_inner(
                &lua,
                &fields,
                &data,
                &ValidationCtx::builder(&conn, "test").build(),
            );
            assert!(result.is_ok(), "input {ok:?} should pass: {result:?}");
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
            let mut data = DocumentFields::new();
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
