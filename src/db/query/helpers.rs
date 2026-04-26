//! Value helpers: pagination limits, date normalization, type coercion.

use anyhow::Result;
use anyhow::anyhow;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::Value;

use crate::{
    core::{FieldDefinition, FieldType},
    db::DbValue,
};

use super::sanitize_locale;

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

/// Normalize a date value using a specific IANA timezone.
/// The input is treated as local time in the given timezone, then converted to UTC.
/// If the input already has a timezone offset (RFC 3339), it is converted directly.
pub fn normalize_date_with_timezone(value: &str, tz_str: &str) -> Result<String> {
    let tz: Tz = tz_str
        .parse()
        .map_err(|_| anyhow!("Invalid timezone: {}", tz_str))?;

    let trimmed = value.trim();

    // Date only: "2024-01-15" -> noon in the given timezone -> UTC
    if trimmed.len() == 10
        && let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
    {
        let local_noon = date
            .and_hms_opt(12, 0, 0)
            .ok_or_else(|| anyhow!("Failed to construct noon time for {}", trimmed))?;

        let utc = tz
            .from_local_datetime(&local_noon)
            .earliest()
            .ok_or_else(|| anyhow!("Invalid local time for {} in {}", trimmed, tz_str))?
            .with_timezone(&Utc);

        return Ok(utc.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string());
    }

    // datetime-local: "2024-01-15T09:00" or "2024-01-15T09:00:00"
    let formats = ["%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M:%S"];

    for fmt in &formats {
        if let Ok(naive) = NaiveDateTime::parse_from_str(trimmed, fmt) {
            let utc = tz
                .from_local_datetime(&naive)
                .earliest()
                .ok_or_else(|| anyhow!("Invalid local time for {} in {}", trimmed, tz_str))?
                .with_timezone(&Utc);

            return Ok(utc.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string());
        }
    }

    // If already has timezone offset (RFC 3339), just normalize to UTC
    Ok(normalize_date_value(value))
}

/// Convert a UTC ISO 8601 date string to local time in the given IANA timezone.
/// Returns the local datetime formatted for `<input type="datetime-local">` (YYYY-MM-DDTHH:MM)
/// or `<input type="date">` (YYYY-MM-DD, using the 10-char prefix).
pub fn utc_to_local(utc_value: &str, tz_str: &str) -> Option<String> {
    let tz: Tz = tz_str.parse().ok()?;
    let trimmed = utc_value.trim();

    // Parse as RFC 3339 / ISO 8601 (stored format: "2024-01-15T12:00:00.000Z")
    let dt = DateTime::<FixedOffset>::parse_from_rfc3339(trimmed)
        .or_else(|_| {
            // Try with space separator (SQLite format)
            DateTime::<FixedOffset>::parse_from_rfc3339(&trimmed.replace(' ', "T"))
        })
        .ok()?;

    let local = dt.with_timezone(&tz);

    Some(local.format("%Y-%m-%dT%H:%M").to_string())
}

/// Reject a text-like value if it contains a NUL byte.
///
/// Applies to `Text`, `Textarea`, and `Email` field types. Other types (numeric,
/// date, etc.) are coerced independently and do not need this guard. The error
/// message mirrors the email-header CRLF validator for consistency.
pub(crate) fn validate_no_null_byte(
    field_type: &FieldType,
    field_name: &str,
    value: &str,
) -> Result<()> {
    let applies = matches!(
        field_type,
        FieldType::Text | FieldType::Textarea | FieldType::Email
    );

    if !applies {
        return Ok(());
    }

    if value.bytes().any(|b| b == 0) {
        return Err(anyhow!(
            "field '{field_name}' contains forbidden control characters"
        ));
    }

    Ok(())
}

/// Coerce a form string value to the appropriate database type.
pub(crate) fn coerce_value(field_type: &FieldType, value: &str) -> DbValue {
    if value.is_empty() && *field_type != FieldType::Checkbox {
        return DbValue::Null;
    }

    match field_type {
        FieldType::Checkbox => {
            DbValue::Integer(matches!(value, "on" | "true" | "1" | "yes") as i64)
        }
        FieldType::Number => value
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite())
            .map(DbValue::Real)
            .unwrap_or(DbValue::Null),
        FieldType::Date => DbValue::Text(normalize_date_value(value)),
        _ => DbValue::Text(value.to_string()),
    }
}

/// Coerce a `serde_json::Value` to the appropriate database type, preserving
/// numeric precision. Unlike [`coerce_value`] (which takes `&str`), this
/// operates on typed JSON values directly — important for backends like
/// Postgres that require typed parameters.
#[allow(dead_code)]
pub(crate) fn coerce_json_value(field_type: &FieldType, val: &Value) -> DbValue {
    match val {
        Value::Null => DbValue::Null,
        Value::Bool(b) => DbValue::Integer(if *b { 1 } else { 0 }),
        Value::Number(n) => match field_type {
            FieldType::Number => DbValue::Real(n.as_f64().unwrap_or(0.0)),
            _ => n
                .as_i64()
                .map(DbValue::Integer)
                .unwrap_or_else(|| DbValue::Real(n.as_f64().unwrap_or(0.0))),
        },
        Value::String(s) => coerce_value(field_type, s),
        Value::Array(arr) => DbValue::Text(Value::Array(arr.clone()).to_string()),
        Value::Object(obj) => DbValue::Text(Value::Object(obj.clone()).to_string()),
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
        .map(DbValue::Text)
        .unwrap_or_else(|_| coerce_value(field_type, value))
}

/// Build a prefixed name: `"prefix__name"` or just `"name"` when prefix is empty.
///
/// Used by field walkers that track group nesting (backfill, back-references, columns).
pub(crate) fn prefixed_name(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{}__{}", prefix, name)
    }
}

/// Walk a field tree, calling `visit` for each non-layout field.
///
/// Handles Group (prefixed recursion with localized propagation),
/// Row/Collapsible (passthrough), and Tabs (per-tab recursion).
/// The visitor receives `(field, prefix, inherited_localized)` and decides
/// what to do — including whether to skip non-parent-column fields.
pub(crate) fn walk_leaf_fields<F>(
    fields: &[FieldDefinition],
    prefix: &str,
    inherited_localized: bool,
    visit: &mut F,
) -> Result<()>
where
    F: FnMut(&FieldDefinition, &str, bool) -> Result<()>,
{
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = prefixed_name(prefix, &field.name);

                walk_leaf_fields(
                    &field.fields,
                    &new_prefix,
                    inherited_localized || field.localized,
                    visit,
                )?;
            }
            FieldType::Row | FieldType::Collapsible => {
                walk_leaf_fields(&field.fields, prefix, inherited_localized, visit)?;
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    walk_leaf_fields(&tab.fields, prefix, inherited_localized, visit)?;
                }
            }
            _ => visit(field, prefix, inherited_localized)?,
        }
    }

    Ok(())
}

/// Build a locale-suffixed column name: `"field__en"`, `"seo__title__de"`.
///
/// Sanitizes the locale string before appending.
pub(crate) fn locale_column(field_name: &str, locale: &str) -> Result<String> {
    Ok(format!("{}__{}", field_name, sanitize_locale(locale)?))
}

/// Current UTC timestamp in ISO 8601 format with milliseconds: `"2024-01-15T14:00:00.000Z"`.
pub(crate) fn utc_now() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S.000Z")
        .to_string()
}

/// Build a timezone companion column name: `"field_tz"`, `"seo__start_tz"`.
pub(crate) fn tz_column(name: &str) -> String {
    format!("{name}_tz")
}

/// Build a code-language companion column name: `"snippet_lang"`,
/// `"meta__example_lang"`. Used by code fields with a non-empty
/// `admin.languages` allow-list — see `apply_code` in the field-context
/// builder.
pub(crate) fn lang_column(name: &str) -> String {
    format!("{name}_lang")
}

/// Build a join table name: `"collection_field"`, `"posts_tags"`.
pub(crate) fn join_table(collection: &str, field: &str) -> String {
    format!("{collection}_{field}")
}

/// Build the table name for a global: `"_global_{slug}"`.
pub(crate) fn global_table(slug: &str) -> String {
    format!("_global_{slug}")
}

/// Append a SQL condition with `WHERE` or `AND` depending on whether a WHERE clause already exists.
pub(crate) fn append_sql_condition(sql: &mut String, has_where: &mut bool, condition: &str) {
    sql.push_str(if *has_where { " AND " } else { " WHERE " });
    sql.push_str(condition);
    *has_where = true;
}

#[cfg(test)]
mod tests {
    use serde_json::json;

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

    #[test]
    fn coerce_value_rejects_null_byte_in_text() {
        // Applies to Text, Textarea, Email.
        for ft in [FieldType::Text, FieldType::Textarea, FieldType::Email] {
            let err = validate_no_null_byte(&ft, "mykey", "hello\0world").unwrap_err();
            let msg = format!("{err}");
            assert!(msg.contains("mykey"), "error should name the field: {msg}");
            assert!(
                msg.contains("forbidden control characters"),
                "error wording: {msg}"
            );
        }

        // Does not apply to Number/Date/Checkbox.
        assert!(validate_no_null_byte(&FieldType::Number, "n", "1\x002").is_ok());
        assert!(validate_no_null_byte(&FieldType::Date, "d", "2024-01-01").is_ok());

        // Clean text passes.
        assert!(validate_no_null_byte(&FieldType::Text, "t", "hello world").is_ok());
        // Empty passes.
        assert!(validate_no_null_byte(&FieldType::Text, "t", "").is_ok());
    }

    #[test]
    fn coerce_value_date_normalizes() {
        assert_eq!(
            coerce_value(&FieldType::Date, "2026-03-15"),
            DbValue::Text("2026-03-15T12:00:00.000Z".into())
        );
    }

    // ── apply_pagination_limits tests ──────────────────────────────────

    // ── coerce_json_value tests ──────────────────────────────────────

    #[test]
    fn coerce_json_null() {
        assert_eq!(
            coerce_json_value(&FieldType::Text, &Value::Null),
            DbValue::Null
        );
    }

    #[test]
    fn coerce_json_bool_true() {
        assert_eq!(
            coerce_json_value(&FieldType::Checkbox, &Value::Bool(true)),
            DbValue::Integer(1)
        );
    }

    #[test]
    fn coerce_json_bool_false() {
        assert_eq!(
            coerce_json_value(&FieldType::Checkbox, &Value::Bool(false)),
            DbValue::Integer(0)
        );
    }

    #[test]
    fn coerce_json_number_as_real_for_number_field() {
        let val = json!(42.5);
        assert_eq!(
            coerce_json_value(&FieldType::Number, &val),
            DbValue::Real(42.5)
        );
    }

    #[test]
    fn coerce_json_integer_for_number_field() {
        let val = json!(42);
        // Number field always yields Real
        assert_eq!(
            coerce_json_value(&FieldType::Number, &val),
            DbValue::Real(42.0)
        );
    }

    #[test]
    fn coerce_json_integer_for_non_number_field() {
        let val = json!(42);
        // Non-number field: integer stays as Integer
        assert_eq!(
            coerce_json_value(&FieldType::Text, &val),
            DbValue::Integer(42)
        );
    }

    #[test]
    fn coerce_json_float_for_non_number_field() {
        let val = json!(3.15);
        // Non-number field, but value has no i64 representation: falls back to Real
        assert_eq!(
            coerce_json_value(&FieldType::Text, &val),
            DbValue::Real(3.15)
        );
    }

    #[test]
    fn coerce_json_string_delegates_to_coerce_value() {
        let val = json!("hello");
        assert_eq!(
            coerce_json_value(&FieldType::Text, &val),
            DbValue::Text("hello".into())
        );
    }

    #[test]
    fn coerce_json_string_empty_is_null() {
        let val = json!("");
        assert_eq!(coerce_json_value(&FieldType::Text, &val), DbValue::Null);
    }

    #[test]
    fn coerce_json_array_to_text() {
        let val = json!([1, 2, 3]);
        assert_eq!(
            coerce_json_value(&FieldType::Text, &val),
            DbValue::Text("[1,2,3]".into())
        );
    }

    #[test]
    fn coerce_json_object_to_text() {
        let val = json!({"key": "value"});
        assert_eq!(
            coerce_json_value(&FieldType::Text, &val),
            DbValue::Text(r#"{"key":"value"}"#.into())
        );
    }

    // ── apply_pagination_limits tests ──────────────────────────────────

    // ── normalize_date_with_timezone tests ───────────────────────────

    #[test]
    fn normalize_date_with_tz_date_only() {
        let result = normalize_date_with_timezone("2024-01-15", "America/New_York").unwrap();
        assert_eq!(result, "2024-01-15T17:00:00.000Z"); // noon EST = 5pm UTC
    }

    #[test]
    fn normalize_date_with_tz_datetime() {
        let result = normalize_date_with_timezone("2024-01-15T09:00", "America/New_York").unwrap();
        assert_eq!(result, "2024-01-15T14:00:00.000Z"); // 9am EST = 2pm UTC
    }

    #[test]
    fn normalize_date_with_tz_sao_paulo() {
        // Sao Paulo in May is UTC-3 (standard time, no DST)
        // 09:00 local = 12:00 UTC
        let result = normalize_date_with_timezone("2026-05-01T09:00", "America/Sao_Paulo").unwrap();
        assert_eq!(result, "2026-05-01T12:00:00.000Z");
    }

    #[test]
    fn normalize_date_with_tz_utc_passthrough() {
        let result = normalize_date_with_timezone("2024-01-15T09:00", "UTC").unwrap();
        assert_eq!(result, "2024-01-15T09:00:00.000Z");
    }

    #[test]
    fn normalize_date_with_tz_invalid_tz() {
        let result = normalize_date_with_timezone("2024-01-15", "Invalid/Zone");
        assert!(result.is_err());
    }

    #[test]
    fn normalize_date_with_tz_already_rfc3339() {
        let result =
            normalize_date_with_timezone("2024-01-15T09:00:00+05:00", "America/New_York").unwrap();
        assert_eq!(result, "2024-01-15T04:00:00.000Z"); // Already has offset, timezone ignored
    }

    // ── utc_to_local tests ────────────────────────────────────────────

    #[test]
    fn utc_to_local_sao_paulo() {
        // 12:00 UTC = 09:00 Sao Paulo (UTC-3)
        let result = utc_to_local("2026-05-01T12:00:00.000Z", "America/Sao_Paulo");
        assert_eq!(result.unwrap(), "2026-05-01T09:00");
    }

    #[test]
    fn utc_to_local_new_york() {
        // 14:00 UTC = 09:00 EST (January, UTC-5)
        let result = utc_to_local("2024-01-15T14:00:00.000Z", "America/New_York");
        assert_eq!(result.unwrap(), "2024-01-15T09:00");
    }

    #[test]
    fn utc_to_local_utc() {
        let result = utc_to_local("2024-01-15T09:00:00.000Z", "UTC");
        assert_eq!(result.unwrap(), "2024-01-15T09:00");
    }

    #[test]
    fn utc_to_local_invalid_tz_returns_none() {
        let result = utc_to_local("2024-01-15T09:00:00.000Z", "Invalid/Zone");
        assert!(result.is_none());
    }

    #[test]
    fn utc_to_local_roundtrip_sao_paulo() {
        // Roundtrip: local → UTC → back to local must be idempotent
        let utc = normalize_date_with_timezone("2026-05-01T09:00", "America/Sao_Paulo").unwrap();
        assert_eq!(utc, "2026-05-01T12:00:00.000Z");

        let local = utc_to_local(&utc, "America/Sao_Paulo").unwrap();
        assert_eq!(local, "2026-05-01T09:00");
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
