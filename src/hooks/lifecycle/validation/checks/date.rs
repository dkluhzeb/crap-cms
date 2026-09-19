use chrono::{DateTime, FixedOffset, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use serde_json::Value;

use crate::core::{FieldDefinition, FieldType, PickerAppearance, validate::FieldError};

/// The shape a date value spells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DateShape {
    /// `HH:MM` or `HH:MM:SS`.
    Time,
    /// `YYYY-MM`.
    Month,
    /// `YYYY-MM-DD`.
    Day,
    /// A datetime: RFC 3339, `YYYY-MM-DDTHH:MM` or `YYYY-MM-DDTHH:MM:SS`.
    DateTime,
}

/// Validate date format and date bounds (`min_date` / `max_date`). A present
/// value must be a string — a number (`starts = 1736899200`) would store as
/// the digits — spelling a shape the field's picker can show: a `timeOnly`
/// picker a time, a `monthOnly` picker a month, a `dayOnly` or `dayAndTime`
/// picker a date or a datetime (the picker cuts a datetime to what it shows).
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

    let Some(value) = value else {
        return;
    };
    let Value::String(s) = value else {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{} must be an ISO-8601 date string", field.name),
                "validation.invalid_date_type",
            )
            .with_param("field", field.name.clone()),
        );

        return;
    };

    match date_shape(s) {
        None => errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{} is not a valid date format", field.name),
                "validation.invalid_date",
            )
            .with_param("field", field.name.clone()),
        ),
        Some(shape) if !appearance_shows(field.picker_appearance.as_ref(), shape) => {
            errors.push(appearance_error(field, data_key));
        }
        Some(_) => {}
    }

    let date_part = s.get(..10).unwrap_or(s.as_str());

    if let Some(ref min_date) = field.min_date
        && date_part < min_date.as_str()
    {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{} must be on or after {}", field.name, min_date),
                "validation.date_min",
            )
            .with_param("field", field.name.clone())
            .with_param("min", min_date.clone()),
        );
    }

    if let Some(ref max_date) = field.max_date
        && date_part > max_date.as_str()
    {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!("{} must be on or before {}", field.name, max_date),
                "validation.date_max",
            )
            .with_param("field", field.name.clone())
            .with_param("max", max_date.clone()),
        );
    }
}

/// Reject a wall-clock time that does not exist in the field's timezone.
///
/// A timezone-enabled Date stores a local time plus its IANA zone. On a
/// spring-forward day the skipped hour (02:30 in Europe/Berlin on the last
/// Sunday of March) names no instant at all; without this check the write
/// silently fell back to storing the wall-clock digits as UTC. Values that
/// carry their own offset, and zones that do not parse, are left to the other
/// checks.
pub(crate) fn check_local_time_exists(
    field: &FieldDefinition,
    data_key: &str,
    value: Option<&Value>,
    tz: Option<&str>,
    errors: &mut Vec<FieldError>,
) {
    if field.field_type != FieldType::Date || !field.timezone {
        return;
    }

    let (Some(Value::String(s)), Some(tz_name)) = (value, tz.filter(|t| !t.is_empty())) else {
        return;
    };
    let Ok(zone) = tz_name.parse::<Tz>() else {
        return;
    };
    let Some(local) = ["%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M:%S"]
        .iter()
        .find_map(|fmt| NaiveDateTime::parse_from_str(s.trim(), fmt).ok())
    else {
        return;
    };

    if matches!(zone.from_local_datetime(&local), LocalResult::None) {
        errors.push(
            FieldError::with_key(
                data_key.to_owned(),
                format!(
                    "{} {s} does not exist in {tz_name} (skipped by a daylight-saving change)",
                    field.name
                ),
                "validation.nonexistent_local_time",
            )
            .with_param("field", field.name.clone())
            .with_param("value", s.clone())
            .with_param("timezone", tz_name.to_owned()),
        );
    }
}

/// Whether a picker of `appearance` (none: the default, `dayOnly`) can show a
/// value of `shape`. A day or datetime picker shows a date or a datetime — the
/// stored form of a day is a datetime at noon UTC, and a datetime is cut to
/// its day; a time picker shows only a time, a month picker only a month.
fn appearance_shows(appearance: Option<&PickerAppearance>, shape: DateShape) -> bool {
    match appearance {
        Some(PickerAppearance::TimeOnly) => shape == DateShape::Time,
        Some(PickerAppearance::MonthOnly) => shape == DateShape::Month,
        Some(PickerAppearance::DayOnly | PickerAppearance::DayAndTime) | None => {
            matches!(shape, DateShape::Day | DateShape::DateTime)
        }
    }
}

/// The error for a value the field's picker cannot show — and so could not
/// round-trip through the admin form.
fn appearance_error(field: &FieldDefinition, data_key: &str) -> FieldError {
    let (expected, appearance) = match field.picker_appearance {
        Some(PickerAppearance::TimeOnly) => ("a time (HH:MM or HH:MM:SS)", "timeOnly"),
        Some(PickerAppearance::MonthOnly) => ("a month (YYYY-MM)", "monthOnly"),
        Some(PickerAppearance::DayAndTime) => ("a date or datetime", "dayAndTime"),
        Some(PickerAppearance::DayOnly) | None => ("a date or datetime", "dayOnly"),
    };

    FieldError::with_key(
        data_key.to_owned(),
        format!(
            "{} must be {expected} for a {appearance} picker",
            field.name
        ),
        "validation.date_shape",
    )
    .with_param("field", field.name.clone())
    .with_param("expected", expected.to_string())
    .with_param("appearance", appearance.to_string())
}

/// The shape a date value spells, or `None` when it spells no date at all.
/// Recognized: YYYY-MM-DD, YYYY-MM-DDTHH:MM, YYYY-MM-DDTHH:MM:SS, full ISO
/// 8601/RFC 3339, HH:MM (time only), HH:MM:SS, YYYY-MM (month only).
fn date_shape(value: &str) -> Option<DateShape> {
    // Time only: HH:MM or HH:MM:SS — range-checked, not just digit shape
    // (`"99:99"` used to pass the digit-only test).
    if value.len() <= 8 && value.contains(':') && !value.contains('T') {
        return is_time(value).then_some(DateShape::Time);
    }

    // Month only: YYYY-MM — the month must be 01-12 (`"2024-99"` used to pass).
    if value.len() == 7 && value.as_bytes().get(4) == Some(&b'-') && !value.contains('T') {
        return is_month(value).then_some(DateShape::Month);
    }

    // Full RFC 3339
    if DateTime::<FixedOffset>::parse_from_rfc3339(value).is_ok() {
        return Some(DateShape::DateTime);
    }

    // Date only: YYYY-MM-DD
    if value.len() == 10 {
        return NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .is_ok()
            .then_some(DateShape::Day);
    }

    // datetime-local: YYYY-MM-DDTHH:MM, or with seconds (no timezone)
    let local = match value.len() {
        16 if value.contains('T') => NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M"),
        19 if value.contains('T') => NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S"),
        _ => return None,
    };

    local.is_ok().then_some(DateShape::DateTime)
}

/// Whether `value` is a time of day: two-digit `HH:MM` or `HH:MM:SS`, in range.
fn is_time(value: &str) -> bool {
    let parts: Vec<&str> = value.split(':').collect();

    if parts.len() != 2 && parts.len() != 3 {
        return false;
    }

    let Some(nums) = parts
        .iter()
        .map(|p| p.parse::<u32>().ok())
        .collect::<Option<Vec<u32>>>()
    else {
        return false;
    };
    let all_two_digit = parts.iter().all(|p| p.len() == 2);
    let ss = nums.get(2).copied().unwrap_or(0);

    all_two_digit && nums[0] < 24 && nums[1] < 60 && ss < 60
}

/// Whether `value` is a month: `YYYY-MM`, the month 01-12.
fn is_month(value: &str) -> bool {
    let parts: Vec<&str> = value.split('-').collect();

    if parts.len() != 2 || parts[0].len() != 4 || parts[1].len() != 2 {
        return false;
    }

    let year_ok = parts[0].chars().all(|c| c.is_ascii_digit());
    let month_ok = parts[1].parse::<u32>().is_ok_and(|m| (1..=12).contains(&m));

    year_ok && month_ok
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::core::DocumentFields;
    use crate::hooks::lifecycle::validation::{ValidationCtx, validate_fields_inner};
    use serde_json::json;

    /// Whether `value` spells any date shape at all.
    fn is_valid_date_format(value: &str) -> bool {
        date_shape(value).is_some()
    }

    /// The errors of validating `value` on a date field with `appearance`.
    fn date_errors(appearance: Option<PickerAppearance>, value: &Value) -> Vec<FieldError> {
        let mut field = FieldDefinition::builder("d", FieldType::Date).build();
        field.picker_appearance = appearance;
        let mut errors = Vec::new();

        check_date_field(&field, "d", Some(value), false, &mut errors);

        errors
    }

    fn error_keys(errors: &[FieldError]) -> Vec<&str> {
        errors.iter().filter_map(|e| e.key.as_deref()).collect()
    }

    /// Regression: a number on a date field (`starts = 1736899200`) passed
    /// validation and stored as its digits. Any present value that isn't a
    /// string is rejected, whatever the picker.
    #[test]
    fn a_non_string_date_value_is_rejected() {
        let values = [
            json!(1_736_899_200),
            json!(true),
            json!(["2024-01-15"]),
            json!({}),
        ];

        for value in values {
            let errors = date_errors(None, &value);
            assert_eq!(
                error_keys(&errors),
                vec!["validation.invalid_date_type"],
                "{value}"
            );
            assert!(errors[0].message.contains("ISO-8601 date string"));
        }

        assert!(date_errors(None, &json!("2024-01-15")).is_empty());
    }

    /// Regression: every date shape passed for every picker, so an API could
    /// store a datetime on a `timeOnly` field — which the editor's input then
    /// blanked, and a no-op save wrote NULL. A picker accepts only a shape it
    /// can show: a time picker a time, a month picker a month, a day or
    /// datetime picker (the default) a date or a datetime.
    #[test]
    fn a_value_the_picker_cannot_show_is_rejected() {
        let shapes = [
            ("14:30", DateShape::Time),
            ("14:30:15", DateShape::Time),
            ("2024-01", DateShape::Month),
            ("2024-01-15", DateShape::Day),
            ("2024-01-15T09:00", DateShape::DateTime),
            ("2024-01-15T09:00:30", DateShape::DateTime),
            ("2024-01-15T12:00:00.000Z", DateShape::DateTime),
        ];
        let pickers = [
            (None, "default"),
            (Some(PickerAppearance::DayOnly), "dayOnly"),
            (Some(PickerAppearance::DayAndTime), "dayAndTime"),
            (Some(PickerAppearance::TimeOnly), "timeOnly"),
            (Some(PickerAppearance::MonthOnly), "monthOnly"),
        ];

        for (value, shape) in shapes {
            assert_eq!(date_shape(value), Some(shape), "{value}");

            for (appearance, name) in &pickers {
                let shown = match appearance {
                    Some(PickerAppearance::TimeOnly) => shape == DateShape::Time,
                    Some(PickerAppearance::MonthOnly) => shape == DateShape::Month,
                    _ => matches!(shape, DateShape::Day | DateShape::DateTime),
                };
                let expected: Vec<&str> = if shown {
                    vec![]
                } else {
                    vec!["validation.date_shape"]
                };

                let errors = date_errors(appearance.clone(), &json!(value));
                assert_eq!(error_keys(&errors), expected, "{value} on {name}");
            }
        }

        let errors = date_errors(Some(PickerAppearance::TimeOnly), &json!("2024-01-15"));
        assert!(errors[0].message.contains("timeOnly picker"), "{errors:?}");
    }

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

    /// Regression: time-only and month-only formats were digit-shape-only,
    /// so out-of-range values slipped through while `"2024-13-01"` (full
    /// date) was correctly rejected. They must be range-checked too.
    #[test]
    fn out_of_range_time_and_month_are_rejected() {
        // Time-only out of range
        assert!(!is_valid_date_format("99:99"));
        assert!(!is_valid_date_format("24:00"));
        assert!(!is_valid_date_format("12:60"));
        assert!(!is_valid_date_format("10:30:60"));
        // Month-only out of range
        assert!(!is_valid_date_format("2024-00"));
        assert!(!is_valid_date_format("2024-13"));
        assert!(!is_valid_date_format("2024-99"));
        // In-range boundaries still pass
        assert!(is_valid_date_format("23:59"));
        assert!(is_valid_date_format("23:59:59"));
        assert!(is_valid_date_format("2024-01"));
        assert!(is_valid_date_format("2024-12"));
    }

    // --- validate_fields_inner integration tests ---

    #[test]
    fn test_validate_date_format_invalid() {
        let lua = mlua::Lua::new();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE test (id TEXT PRIMARY KEY, d TEXT)")
            .unwrap();
        let fields = vec![FieldDefinition::builder("d", FieldType::Date).build()];
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
                .picker_appearance(PickerAppearance::MonthOnly)
                .build(),
        ];
        let mut data = DocumentFields::new();
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
        let mut data = DocumentFields::new();
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
                .picker_appearance(PickerAppearance::MonthOnly)
                .build(),
        ];
        let mut data = DocumentFields::new();
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

    /// 02:30 on 2024-03-31 was skipped in Europe/Berlin; 03:30 exists, and a
    /// value with its own offset is not a local time at all.
    #[test]
    fn a_local_time_inside_a_dst_gap_is_rejected() {
        let field = FieldDefinition::builder("starts", FieldType::Date)
            .timezone(true)
            .build();
        let check = |raw: &str| {
            let mut errors = Vec::new();
            let value = Value::String(raw.to_string());
            check_local_time_exists(
                &field,
                "starts",
                Some(&value),
                Some("Europe/Berlin"),
                &mut errors,
            );
            errors
        };

        let gap = check("2024-03-31T02:30");
        assert_eq!(gap.len(), 1);
        assert_eq!(
            gap[0].key.as_deref(),
            Some("validation.nonexistent_local_time")
        );

        assert!(check("2024-03-31T03:30").is_empty());
        assert!(check("2024-03-31T02:30:00+01:00").is_empty());
    }
}
