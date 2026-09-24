//! Form-string coercion to database values.

use crate::{
    core::{FieldType, normalize_email, normalize_text, parse_number, parse_truthy},
    db::{
        DbValue,
        query::helpers::{normalize_date_value, normalize_date_with_timezone},
    },
};

/// Coerce a form string value to the appropriate database type.
pub(crate) fn coerce_value(field_type: &FieldType, value: &str) -> DbValue {
    if value.is_empty() && *field_type != FieldType::Checkbox {
        return DbValue::Null;
    }

    match field_type {
        FieldType::Checkbox => DbValue::Integer(i64::from(parse_truthy(value))),
        FieldType::Number => parse_number(value)
            .filter(|f| f.is_finite())
            .map_or(DbValue::Null, DbValue::Real),
        // Email and text are stored in canonical form, so login, uniqueness and
        // filters compare one spelling of each value however it was typed.
        FieldType::Email => DbValue::Text(normalize_email(value)),
        FieldType::Text | FieldType::Textarea => DbValue::Text(normalize_text(value)),
        FieldType::Date => DbValue::Text(normalize_date_value(value)),
        _ => DbValue::Text(value.to_string()),
    }
}

/// Coerce a date value with optional timezone normalization.
///
/// If the field is a Date with timezone enabled and a non-empty timezone string is provided,
/// normalizes the value using that timezone. Falls back to plain `coerce_value` when
/// no timezone is available or on normalization error.
pub(crate) fn coerce_date_value(field_type: &FieldType, value: &str, tz: Option<&str>) -> DbValue {
    let tz = match tz.filter(|s| !s.is_empty()) {
        Some(tz) if *field_type == FieldType::Date => tz,
        _ => return coerce_value(field_type, value),
    };

    if value.is_empty() {
        return DbValue::Null;
    }

    normalize_date_with_timezone(value, tz)
        .map_or_else(|_| coerce_value(field_type, value), DbValue::Text)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── coerce_value tests ─────────────────────────────────────────────

    #[test]
    fn coerce_value_checkbox_truthy() {
        for input in &["on", "true", "1", "yes"] {
            assert_eq!(
                coerce_value(&FieldType::Checkbox, input),
                DbValue::Integer(1),
                "Expected Integer(1) for checkbox input '{input}'"
            );
        }
    }

    #[test]
    fn coerce_value_checkbox_falsy() {
        for input in &["off", "false", "0", "no"] {
            assert_eq!(
                coerce_value(&FieldType::Checkbox, input),
                DbValue::Integer(0),
                "Expected Integer(0) for checkbox input '{input}'"
            );
        }
    }

    #[test]
    fn coerce_value_number_valid() {
        let val = coerce_value(&FieldType::Number, "42.5");
        assert_eq!(val, DbValue::Real(42.5));
    }

    #[test]
    fn coerce_value_number_empty_is_null() {
        assert_eq!(coerce_value(&FieldType::Number, ""), DbValue::Null);
    }

    #[test]
    fn coerce_value_number_invalid_is_null() {
        assert_eq!(coerce_value(&FieldType::Number, "abc"), DbValue::Null);
    }

    #[test]
    fn coerce_value_number_nan_is_null() {
        assert_eq!(coerce_value(&FieldType::Number, "NaN"), DbValue::Null);
    }

    #[test]
    fn coerce_value_number_infinity_is_null() {
        assert_eq!(coerce_value(&FieldType::Number, "inf"), DbValue::Null);
        assert_eq!(coerce_value(&FieldType::Number, "infinity"), DbValue::Null);
        assert_eq!(coerce_value(&FieldType::Number, "-inf"), DbValue::Null);
    }

    #[test]
    fn coerce_value_text_nonempty() {
        assert_eq!(
            coerce_value(&FieldType::Text, "hello"),
            DbValue::Text("hello".into())
        );
    }

    #[test]
    fn coerce_value_text_empty_is_null() {
        assert_eq!(coerce_value(&FieldType::Text, ""), DbValue::Null);
    }

    #[test]
    fn coerce_value_date_empty_is_null() {
        assert_eq!(coerce_value(&FieldType::Date, ""), DbValue::Null);
    }

    /// Email and text reach storage in one canonical form, whichever way the
    /// same characters were typed.
    #[test]
    fn coerce_value_stores_canonical_email_and_text() {
        assert_eq!(
            coerce_value(&FieldType::Email, "  ANGE\u{300}LE@J\u{dc}RGEN.example "),
            DbValue::Text("ang\u{e8}le@j\u{fc}rgen.example".into())
        );

        for ft in [FieldType::Text, FieldType::Textarea] {
            assert_eq!(
                coerce_value(&ft, "Cafe\u{301} Cr\u{e8}me"),
                DbValue::Text("Caf\u{e9} Cr\u{e8}me".into()),
                "{ft:?} keeps case but composes accents"
            );
        }
    }

    #[test]
    fn coerce_value_date_normalizes() {
        assert_eq!(
            coerce_value(&FieldType::Date, "2026-03-15"),
            DbValue::Text("2026-03-15T12:00:00.000Z".into())
        );
    }
}
