use std::collections::HashMap;

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime};
use chrono_tz::Tz;
use serde_json::Value;

use crate::core::{FieldDefinition, FieldType, validate::FieldError};

/// Validate date format and date bounds (min_date / max_date).
pub(crate) fn check_date_field(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    is_empty: bool,
    errors: &mut Vec<FieldError>,
) {
    if field.field_type != FieldType::Date || is_empty {
        return;
    }

    let Some(Value::String(s)) = value else {
        return;
    };

    if !is_valid_date_format(s) {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{} is not a valid date format", field.name),
            "validation.invalid_date",
            HashMap::from([("field".to_string(), field.name.clone())]),
        ));
    }

    let date_part = s.get(..10).unwrap_or(s.as_str());

    if let Some(ref min_date) = field.min_date
        && date_part < min_date.as_str()
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{} must be on or after {}", field.name, min_date),
            "validation.date_min",
            HashMap::from([
                ("field".to_string(), field.name.clone()),
                ("min".to_string(), min_date.clone()),
            ]),
        ));
    }

    if let Some(ref max_date) = field.max_date
        && date_part > max_date.as_str()
    {
        errors.push(FieldError::with_key(
            data_key.to_owned(),
            format!("{} must be on or before {}", field.name, max_date),
            "validation.date_max",
            HashMap::from([
                ("field".to_string(), field.name.clone()),
                ("max".to_string(), max_date.clone()),
            ]),
        ));
    }
}

/// Check if a string is a recognized date format for the date field type.
/// Accepts: YYYY-MM-DD, YYYY-MM-DDTHH:MM, YYYY-MM-DDTHH:MM:SS, full ISO 8601/RFC 3339,
/// HH:MM (time only), HH:MM:SS, YYYY-MM (month only).
pub(crate) fn is_valid_date_format(value: &str) -> bool {
    // Time only: HH:MM or HH:MM:SS
    if value.len() <= 8 && value.contains(':') && !value.contains('T') {
        let parts: Vec<&str> = value.split(':').collect();

        if parts.len() >= 2 {
            return parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()));
        }
    }

    // Month only: YYYY-MM
    if value.len() == 7 && value.as_bytes().get(4) == Some(&b'-') && !value.contains('T') {
        let parts: Vec<&str> = value.split('-').collect();

        if parts.len() == 2 {
            return parts[0].len() == 4
                && parts[0].chars().all(|c| c.is_ascii_digit())
                && parts[1].len() == 2
                && parts[1].chars().all(|c| c.is_ascii_digit());
        }
    }

    // Full RFC 3339
    if DateTime::<FixedOffset>::parse_from_rfc3339(value).is_ok() {
        return true;
    }

    // Date only: YYYY-MM-DD
    if value.len() == 10 {
        return NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok();
    }

    // datetime-local: YYYY-MM-DDTHH:MM
    if value.len() == 16 && value.contains('T') {
        return NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M").is_ok();
    }

    // YYYY-MM-DDTHH:MM:SS (no timezone)
    if value.len() == 19 && value.contains('T') {
        return NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").is_ok();
    }

    false
}

/// Validate that the timezone string is a valid IANA timezone.
#[allow(dead_code)]
pub fn validate_timezone(tz: &str) -> bool {
    tz.parse::<Tz>().is_ok()
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;
    use std::collections::HashMap;

    // --- is_valid_date_format tests ---

    #[test]
    fn test_valid_date_format_date_only() {
        assert!(is_valid_date_format("2024-01-15"));
        assert!(is_valid_date_format("2000-12-31"));
        assert!(is_valid_date_format("1999-06-01"));
    }

    #[test]
    fn test_valid_date_format_datetime_local() {
        assert!(is_valid_date_format("2024-01-15T10:30"));
        assert!(is_valid_date_format("2024-12-31T23:59"));
    }

    #[test]
    fn test_valid_date_format_datetime_seconds() {
        assert!(is_valid_date_format("2024-01-15T10:30:45"));
        assert!(is_valid_date_format("2024-12-31T23:59:59"));
    }

    #[test]
    fn test_valid_date_format_rfc3339() {
        assert!(is_valid_date_format("2024-01-15T10:30:00+00:00"));
        assert!(is_valid_date_format("2024-01-15T10:30:00Z"));
        assert!(is_valid_date_format("2024-01-15T10:30:00-05:00"));
    }

    #[test]
    fn test_valid_date_format_time_only() {
        assert!(is_valid_date_format("10:30"));
        assert!(is_valid_date_format("23:59"));
        assert!(is_valid_date_format("00:00"));
        assert!(is_valid_date_format("10:30:45"));
    }

    #[test]
    fn test_valid_date_format_month_only() {
        assert!(is_valid_date_format("2024-01"));
        assert!(is_valid_date_format("2024-12"));
        assert!(is_valid_date_format("1999-06"));
    }

    #[test]
    fn test_invalid_date_format() {
        assert!(!is_valid_date_format(""));
        assert!(!is_valid_date_format("not-a-date"));
        assert!(!is_valid_date_format("2024"));
        assert!(!is_valid_date_format("2024-1-1"));
        assert!(!is_valid_date_format("01/15/2024"));
        assert!(!is_valid_date_format("2024-13-01")); // invalid month
        assert!(!is_valid_date_format("2024-01-32")); // invalid day
    }

    #[test]
    fn test_valid_date_format_time_only_with_seconds() {
        assert!(is_valid_date_format("10:30:45"));
        assert!(is_valid_date_format("00:00:00"));
        assert!(is_valid_date_format("23:59:59"));
    }

    #[test]
    fn test_invalid_date_format_time_like_but_non_digit() {
        assert!(!is_valid_date_format("ab:cd"));
        assert!(!is_valid_date_format("1a:30"));
    }

    // --- validate_timezone tests ---

    #[test]
    fn test_validate_timezone_valid() {
        assert!(validate_timezone("UTC"));
        assert!(validate_timezone("America/New_York"));
        assert!(validate_timezone("Europe/London"));
        assert!(validate_timezone("Asia/Tokyo"));
    }

    #[test]
    fn test_validate_timezone_invalid() {
        assert!(!validate_timezone("Invalid/Zone"));
        assert!(!validate_timezone(""));
        assert!(!validate_timezone("NotATimezone"));
    }

    // --- validate_fields_inner integration tests ---

    #[test]
    fn test_validate_date_format_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, d TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("d", FieldType::Date).build()];
        let mut data = HashMap::new();
        data.insert("d".to_string(), json!("not-a-date"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().errors[0].message.contains("valid date"));
    }

    #[test]
    fn test_validate_date_format_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, d TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("d", FieldType::Date).build()];
        let mut data = HashMap::new();
        data.insert("d".to_string(), json!("2024-01-15"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_date_min_date_valid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, start_date TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .min_date("2024-01-01")
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("start_date".to_string(), json!("2024-06-15T12:00:00.000Z"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(result.is_ok(), "Date after min_date should pass");
    }

    #[test]
    fn test_validate_date_min_date_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, start_date TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("start_date", FieldType::Date)
                .min_date("2024-06-01")
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("start_date".to_string(), json!("2024-01-15T12:00:00.000Z"));
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
                .contains("on or after")
        );
    }

    #[test]
    fn test_validate_date_max_date_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, end_date TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("end_date", FieldType::Date)
                .max_date("2025-12-31")
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("end_date".to_string(), json!("2026-03-15T12:00:00.000Z"));
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
                .contains("on or before")
        );
    }

    #[test]
    fn test_validate_date_bounds_empty_passes() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, d TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("d", FieldType::Date)
                .min_date("2024-01-01")
                .max_date("2025-12-31")
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("d".to_string(), json!(""));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_ok(),
            "Empty date with bounds should pass (not required)"
        );
    }

    #[test]
    fn test_validate_date_bounds_short_date_min() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, d TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("d", FieldType::Date)
                .min_date("2024-06")
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("d".to_string(), json!("2024-01"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Month-only date before min_date should fail"
        );
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("on or after")
        );
    }

    /// Regression test: date string slicing with multi-byte UTF-8 must not panic.
    /// Previously used `&s[..10]` which panics on non-ASCII; now uses `.get(..10)`.
    #[test]
    fn test_validate_date_bounds_multibyte_does_not_panic() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, d TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("d", FieldType::Date)
                .min_date("2024-01-01")
                .build(),
        ];
        let mut data = HashMap::new();
        // Multi-byte string that would panic with &s[..10] byte slicing
        data.insert(
            "d".to_string(),
            json!("\u{00e9}\u{00e9}\u{00e9}\u{00e9}\u{00e9}"),
        );
        // Should not panic — just produce a validation error
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Invalid date should produce an error, not panic"
        );
    }

    #[test]
    fn test_validate_date_bounds_short_date_max() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, d TEXT)")
            .unwrap();
        let fields = vec![
            FieldDefinition::builder("d", FieldType::Date)
                .max_date("2024-06")
                .build(),
        ];
        let mut data = HashMap::new();
        data.insert("d".to_string(), json!("2024-12"));
        let result = validate_fields_inner(
            &lua,
            &fields,
            &data,
            &ValidationCtx::builder(&conn, "test").build(),
        );
        assert!(
            result.is_err(),
            "Month-only date after max_date should fail"
        );
        assert!(
            result.unwrap_err().errors[0]
                .message
                .contains("on or before")
        );
    }
}
