//! Value helpers: pagination limits, date normalization, type coercion.

use crate::{core::FieldType, db::DbValue};

/// Clamp a requested limit to the configured default/max.
///
/// - `None` → `default_limit`
/// - `Some(v)` → clamped to `[1, max_limit]`
pub fn apply_pagination_limits(requested: Option<i64>, default_limit: i64, max_limit: i64) -> i64 {
    match requested {
        None => default_limit,
        Some(v) => v.max(1).min(max_limit),
    }
}

/// Normalize a date value for storage.
///
/// - Full ISO 8601 with timezone (`2026-01-15T09:00:00Z`, `2026-01-15T09:00:00+05:00`)
///   → re-format as `YYYY-MM-DDTHH:MM:SS.000Z` (UTC)
/// - Date only (`2026-01-15`) → `2026-01-15T12:00:00.000Z` (UTC noon, prevents timezone drift)
/// - datetime-local format (`2026-01-15T09:00`) → treat as UTC → `2026-01-15T09:00:00.000Z`
/// - Time only (`14:30`) → passthrough
/// - Month only (`2026-01`) → passthrough
/// - Anything else → passthrough (validation catches garbage)
pub fn normalize_date_value(value: &str) -> String {
    use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, Utc};

    // Time only: HH:MM or HH:MM:SS
    if value.len() <= 8 && value.contains(':') && !value.contains('T') {
        return value.to_string();
    }

    // Month only: YYYY-MM (exactly 7 chars, dash at position 4)
    if value.len() == 7 && value.as_bytes().get(4) == Some(&b'-') && !value.contains('T') {
        return value.to_string();
    }

    // Try full RFC 3339 / ISO 8601 with timezone (e.g., 2026-01-15T09:00:00Z, 2026-01-15T09:00:00+05:00)
    if let Ok(dt) = DateTime::<FixedOffset>::parse_from_rfc3339(value) {
        let utc = dt.with_timezone(&Utc);

        return utc.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    }

    // Try date only: YYYY-MM-DD (10 chars)
    if value.len() == 10
        && let Ok(d) = NaiveDate::parse_from_str(value, "%Y-%m-%d")
    {
        let noon = d.and_hms_opt(12, 0, 0).expect("12:00:00 is valid");

        return noon.and_utc().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    }

    // Try datetime-local format: YYYY-MM-DDTHH:MM (16 chars, no timezone)
    if value.len() == 16
        && value.contains('T')
        && let Ok(ndt) = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M")
    {
        return ndt.and_utc().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    }

    // Try datetime without timezone: YYYY-MM-DDTHH:MM:SS (19 chars)
    if value.len() == 19
        && value.contains('T')
        && let Ok(ndt) = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S")
    {
        return ndt.and_utc().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    }

    // Anything else: passthrough
    value.to_string()
}

/// Coerce a form string value to the appropriate database type.
pub(crate) fn coerce_value(field_type: &FieldType, value: &str) -> DbValue {
    match field_type {
        FieldType::Checkbox => {
            let b = matches!(value, "on" | "true" | "1" | "yes");
            DbValue::Integer(b as i64)
        }
        FieldType::Number => {
            if value.is_empty() {
                DbValue::Null
            } else if let Ok(f) = value.parse::<f64>() {
                DbValue::Real(f)
            } else {
                DbValue::Null
            }
        }
        FieldType::Date => {
            if value.is_empty() {
                DbValue::Null
            } else {
                DbValue::Text(normalize_date_value(value))
            }
        }
        _ => {
            if value.is_empty() {
                DbValue::Null
            } else {
                DbValue::Text(value.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── normalize_date_value tests ──────────────────────────────────────

    #[test]
    fn normalize_date_only_to_utc_noon() {
        assert_eq!(
            normalize_date_value("2026-01-15"),
            "2026-01-15T12:00:00.000Z"
        );
    }

    #[test]
    fn normalize_full_iso_utc() {
        assert_eq!(
            normalize_date_value("2026-01-15T09:00:00Z"),
            "2026-01-15T09:00:00.000Z"
        );
    }

    #[test]
    fn normalize_iso_with_millis() {
        assert_eq!(
            normalize_date_value("2026-01-15T09:00:00.000Z"),
            "2026-01-15T09:00:00.000Z"
        );
    }

    #[test]
    fn normalize_iso_with_offset() {
        assert_eq!(
            normalize_date_value("2026-01-15T09:00:00+05:00"),
            "2026-01-15T04:00:00.000Z"
        );
    }

    #[test]
    fn normalize_datetime_local() {
        assert_eq!(
            normalize_date_value("2026-01-15T09:00"),
            "2026-01-15T09:00:00.000Z"
        );
    }

    #[test]
    fn normalize_datetime_no_tz() {
        assert_eq!(
            normalize_date_value("2026-01-15T09:00:00"),
            "2026-01-15T09:00:00.000Z"
        );
    }

    #[test]
    fn normalize_time_only_passthrough() {
        assert_eq!(normalize_date_value("14:30"), "14:30");
    }

    #[test]
    fn normalize_month_only_passthrough() {
        assert_eq!(normalize_date_value("2026-01"), "2026-01");
    }

    #[test]
    fn normalize_garbage_passthrough() {
        assert_eq!(normalize_date_value("garbage"), "garbage");
    }

    // ── coerce_value tests ─────────────────────────────────────────────

    #[test]
    fn coerce_value_checkbox_truthy() {
        for input in &["on", "true", "1", "yes"] {
            assert_eq!(
                coerce_value(&FieldType::Checkbox, input),
                DbValue::Integer(1),
                "Expected Integer(1) for checkbox input '{}'",
                input
            );
        }
    }

    #[test]
    fn coerce_value_checkbox_falsy() {
        for input in &["off", "false", "0", "no"] {
            assert_eq!(
                coerce_value(&FieldType::Checkbox, input),
                DbValue::Integer(0),
                "Expected Integer(0) for checkbox input '{}'",
                input
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

    #[test]
    fn coerce_value_date_normalizes() {
        assert_eq!(
            coerce_value(&FieldType::Date, "2026-03-15"),
            DbValue::Text("2026-03-15T12:00:00.000Z".into())
        );
    }

    // ── apply_pagination_limits tests ──────────────────────────────────

    #[test]
    fn pagination_limits_default_when_none() {
        assert_eq!(apply_pagination_limits(None, 100, 1000), 100);
    }

    #[test]
    fn pagination_limits_clamp_max() {
        assert_eq!(apply_pagination_limits(Some(5000), 100, 1000), 1000);
    }

    #[test]
    fn pagination_limits_minimum_one() {
        assert_eq!(apply_pagination_limits(Some(0), 100, 1000), 1);
        assert_eq!(apply_pagination_limits(Some(-5), 100, 1000), 1);
    }

    #[test]
    fn pagination_limits_passthrough() {
        assert_eq!(apply_pagination_limits(Some(50), 100, 1000), 50);
    }
}
