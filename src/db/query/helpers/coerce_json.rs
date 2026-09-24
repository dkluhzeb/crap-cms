//! Typed JSON value coercion to database values.

use serde_json::Value;

use crate::{
    core::{FieldType, json_truthy},
    db::{
        DbValue,
        query::helpers::{coerce_date_value, coerce_value},
    },
};

/// Coerce a typed `serde_json::Value` to the appropriate database type.
///
/// Typed values take a direct path where stringification would change their
/// meaning:
/// - `Number` field × `Value::Number` → `Real` directly (skip parse).
/// - `Checkbox` field × `Value::Bool` / `Value::Number` → `Integer(0|1)` by
///   [`json_truthy`] — any number other than zero is checked, as it is inside a
///   JSON-stored row ([`nested_value`]).
///
/// For all other combinations falls through to stringify + `coerce_value`,
/// which holds the canonical per-field-type semantics (empty-string ⇒ Null,
/// date normalization, checkbox truthy-string match, number parse). This
/// keeps cross-type coercion correct: e.g. `Bool(true)` to a `Text` field
/// becomes `Text("true")`, not `Integer(1)`.
///
/// [`nested_value`]: crate::db::query::helpers::nested_value
pub(crate) fn coerce_json_value(field_type: &FieldType, val: &Value) -> DbValue {
    match (field_type, val) {
        (FieldType::Number, Value::Number(n)) => return DbValue::Real(n.as_f64().unwrap_or(0.0)),
        (FieldType::Checkbox, Value::Bool(_) | Value::Number(_)) => {
            return DbValue::Integer(i64::from(json_truthy(val)));
        }
        _ => {}
    }

    match val {
        Value::Null => DbValue::Null,
        Value::String(s) => coerce_value(field_type, s),
        Value::Bool(b) => coerce_value(field_type, &b.to_string()),
        Value::Number(n) => coerce_value(field_type, &n.to_string()),
        Value::Array(arr) => coerce_value(field_type, &Value::Array(arr.clone()).to_string()),
        Value::Object(obj) => coerce_value(field_type, &Value::Object(obj.clone()).to_string()),
    }
}

/// Value-aware date+tz coercion. Date inputs flow as `Value::String` (the
/// admin form, gRPC `string` proto field, and Lua all serialize date input
/// as a string), so the typed path delegates to [`coerce_date_value`] when
/// the value is a string and falls back to plain [`coerce_json_value`]
/// otherwise.
pub(crate) fn coerce_date_value_json(
    field_type: &FieldType,
    value: &Value,
    tz: Option<&str>,
) -> DbValue {
    let Some(s) = value.as_str() else {
        return coerce_json_value(field_type, value);
    };

    coerce_date_value(field_type, s, tz)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    // ── coerce_json_value tests ──────────────────────────────────────

    #[test]
    fn coerce_json_null_is_null_for_any_field() {
        for ft in [
            FieldType::Text,
            FieldType::Number,
            FieldType::Checkbox,
            FieldType::Date,
        ] {
            assert_eq!(coerce_json_value(&ft, &Value::Null), DbValue::Null);
        }
    }

    // Checkbox field — fast path for typed Bool, string fallback for str input.
    #[test]
    fn coerce_json_checkbox_bool_true() {
        assert_eq!(
            coerce_json_value(&FieldType::Checkbox, &Value::Bool(true)),
            DbValue::Integer(1)
        );
    }

    #[test]
    fn coerce_json_checkbox_bool_false() {
        assert_eq!(
            coerce_json_value(&FieldType::Checkbox, &Value::Bool(false)),
            DbValue::Integer(0)
        );
    }

    #[test]
    fn coerce_json_checkbox_string_truthy() {
        assert_eq!(
            coerce_json_value(&FieldType::Checkbox, &json!("on")),
            DbValue::Integer(1)
        );
        assert_eq!(
            coerce_json_value(&FieldType::Checkbox, &json!("true")),
            DbValue::Integer(1)
        );
    }

    // Number field — fast path for typed Number preserves precision.
    #[test]
    fn coerce_json_number_typed_preserves_real() {
        assert_eq!(
            coerce_json_value(&FieldType::Number, &json!(42.5)),
            DbValue::Real(42.5)
        );
    }

    #[test]
    fn coerce_json_number_integer_typed_yields_real() {
        // Number field always yields Real, even for integer input.
        assert_eq!(
            coerce_json_value(&FieldType::Number, &json!(42)),
            DbValue::Real(42.0)
        );
    }

    #[test]
    fn coerce_json_number_string_parses() {
        assert_eq!(
            coerce_json_value(&FieldType::Number, &json!("42.5")),
            DbValue::Real(42.5)
        );
    }

    #[test]
    fn coerce_json_number_bool_is_null() {
        // Bool isn't a valid number — stringification path goes through
        // coerce_value("true") → parse fail → Null.
        assert_eq!(
            coerce_json_value(&FieldType::Number, &Value::Bool(true)),
            DbValue::Null
        );
    }

    // Text-storing fields — stringify and route through coerce_value.
    #[test]
    fn coerce_json_text_bool_stringifies() {
        // Regression: Bool to a Text field must produce Text("true"),
        // not Integer(1) — the original variant-first dispatch had this bug.
        assert_eq!(
            coerce_json_value(&FieldType::Text, &Value::Bool(true)),
            DbValue::Text("true".into())
        );
    }

    #[test]
    fn coerce_json_text_number_stringifies() {
        // Regression: Number to a Text field must produce Text("42"),
        // not Integer(42).
        assert_eq!(
            coerce_json_value(&FieldType::Text, &json!(42)),
            DbValue::Text("42".into())
        );
    }

    #[test]
    fn coerce_json_text_string_passes_through() {
        assert_eq!(
            coerce_json_value(&FieldType::Text, &json!("hello")),
            DbValue::Text("hello".into())
        );
    }

    #[test]
    fn coerce_json_text_empty_string_is_null() {
        assert_eq!(
            coerce_json_value(&FieldType::Text, &json!("")),
            DbValue::Null
        );
    }

    #[test]
    fn coerce_json_text_array_to_json_text() {
        assert_eq!(
            coerce_json_value(&FieldType::Text, &json!([1, 2, 3])),
            DbValue::Text("[1,2,3]".into())
        );
    }

    #[test]
    fn coerce_json_text_object_to_json_text() {
        assert_eq!(
            coerce_json_value(&FieldType::Text, &json!({"key": "value"})),
            DbValue::Text(r#"{"key":"value"}"#.into())
        );
    }

    #[test]
    fn coerce_json_json_field_object_passes_through() {
        // A Json field stores serialized JSON in a TEXT column; an Object
        // input round-trips as the JSON string.
        assert_eq!(
            coerce_json_value(&FieldType::Json, &json!({"a": 1})),
            DbValue::Text(r#"{"a":1}"#.into())
        );
    }

    // Date field — string input gets normalized; non-string input goes to Null.
    #[test]
    fn coerce_json_date_string_is_normalized() {
        // Day-only input lands at noon UTC (matches `coerce_value` /
        // `normalize_date_value` semantics — see tests above).
        assert_eq!(
            coerce_json_value(&FieldType::Date, &json!("2024-01-15")),
            DbValue::Text("2024-01-15T12:00:00.000Z".into())
        );
    }

    /// Regression: a checkbox's number column went through the string
    /// spellings, so `2` and even `1.0` stored unchecked. Any number other than
    /// zero is checked — the rule `json_truthy` states.
    #[test]
    fn a_checkbox_number_column_follows_json_truthy() {
        for n in [json!(2), json!(1.0), json!(0.5), json!(0), json!(-1)] {
            assert_eq!(
                coerce_json_value(&FieldType::Checkbox, &n),
                DbValue::Integer(i64::from(json_truthy(&n))),
                "{n}"
            );
        }
    }

    /// Regression: a number with surrounding whitespace stored NULL, though a
    /// has-many element with the same spelling stores the number. A non-finite
    /// number still stores nothing.
    #[test]
    fn a_padded_number_stores_the_number() {
        for padded in [" 5", "5 ", "\t5\n"] {
            assert_eq!(
                coerce_value(&FieldType::Number, padded),
                DbValue::Real(5.0),
                "{padded:?}"
            );
            assert_eq!(
                coerce_json_value(&FieldType::Number, &json!(padded)),
                DbValue::Real(5.0),
                "{padded:?}"
            );
        }

        for non_finite in [" inf", "NaN ", " -infinity "] {
            assert_eq!(
                coerce_value(&FieldType::Number, non_finite),
                DbValue::Null,
                "{non_finite:?}"
            );
        }
    }
}
