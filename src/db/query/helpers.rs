//! Value helpers: pagination limits, date normalization, type coercion.

use anyhow::Result;
use anyhow::anyhow;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::Value;

use crate::{
    core::{FieldType, parse_truthy},
    db::{DbValue, types::real_to_json_number},
};

use super::sanitize_locale;

/// Clamp a requested limit to the configured default/max.
///
/// - `None` → `default_limit`
/// - `Some(v)` → clamped to `[1, max_limit]`
#[must_use]
pub fn apply_pagination_limits(requested: Option<i64>, default_limit: i64, max_limit: i64) -> i64 {
    match requested {
        None => default_limit,
        Some(v) => v.max(1).min(max_limit),
    }
}

/// Resolve a requested relationship-population depth into `[0, max_depth]`.
///
/// `None` (no explicit depth) uses the configured `default_depth`; a negative
/// value floors to 0; everything is capped at `max_depth`. One helper so every
/// read surface (Lua / gRPC / MCP / admin) resolves depth identically — the
/// surfaces previously diverged (some defaulted to `default_depth`, some to 0,
/// MCP never floored a negative), so the `[depth] default_depth` knob was
/// honored inconsistently.
#[must_use]
pub fn clamp_depth(requested: Option<i32>, default_depth: i32, max_depth: i32) -> i32 {
    requested.unwrap_or(default_depth).max(0).min(max_depth)
}

/// Floor an optional `limit`/`offset` at 0, preserving `None`.
///
/// Used where `None` is an intended, separately-bounded "no explicit limit"
/// contract (e.g. version history, capped by `max_versions`): we must not turn
/// `None` into a cap, but a negative `Some(-1)` must never become an unbounded
/// `LIMIT -1` read (which `SQLite` treats as *no limit* — a fail-open bypass).
/// The floor at 0 is fail-closed (`LIMIT 0` / `OFFSET 0`). Lives here in
/// `db::query` so every read surface (Lua / gRPC / MCP) and the service layer
/// share one floor without a layering inversion.
#[must_use]
pub fn floor_optional_limit(limit: Option<i64>) -> Option<i64> {
    limit.map(|l| l.max(0))
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
pub(crate) fn normalize_date_value(value: &str) -> String {
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
fn normalize_date_with_timezone(value: &str, tz_str: &str) -> Result<String> {
    let tz: Tz = tz_str
        .parse()
        .map_err(|_| anyhow!("Invalid timezone: {tz_str}"))?;

    let trimmed = value.trim();

    // Date only: "2024-01-15" -> noon in the given timezone -> UTC
    if trimmed.len() == 10
        && let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d")
    {
        let local_noon = date
            .and_hms_opt(12, 0, 0)
            .ok_or_else(|| anyhow!("Failed to construct noon time for {trimmed}"))?;

        let utc = tz
            .from_local_datetime(&local_noon)
            .earliest()
            .ok_or_else(|| anyhow!("Invalid local time for {trimmed} in {tz_str}"))?
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
                .ok_or_else(|| anyhow!("Invalid local time for {trimmed} in {tz_str}"))?
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

/// Value-aware null-byte guard: only checks string-typed values. Non-string
/// JSON values (Number, Bool, Null, Array, Object) cannot carry null bytes.
pub(crate) fn validate_no_null_byte_json(
    field_type: &FieldType,
    field_name: &str,
    value: &Value,
) -> Result<()> {
    if let Some(s) = value.as_str() {
        validate_no_null_byte(field_type, field_name, s)?;
    }

    Ok(())
}

/// Coerce a form string value to the appropriate database type.
pub(crate) fn coerce_value(field_type: &FieldType, value: &str) -> DbValue {
    if value.is_empty() && *field_type != FieldType::Checkbox {
        return DbValue::Null;
    }

    match field_type {
        FieldType::Checkbox => DbValue::Integer(i64::from(parse_truthy(value))),
        FieldType::Number => value
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite())
            .map_or(DbValue::Null, DbValue::Real),
        // Trim email on the way in so stored values share the normal form the
        // case-insensitive login lookup / rate-limit keys compare against — a
        // surrounding-whitespace email otherwise stored but never matched.
        FieldType::Email => DbValue::Text(value.trim().to_string()),
        FieldType::Date => DbValue::Text(normalize_date_value(value)),
        _ => DbValue::Text(value.to_string()),
    }
}

/// Coerce a typed `serde_json::Value` to the appropriate database type.
///
/// Takes a fast path for inputs whose precision would be lost by
/// stringification + reparse:
/// - `Number` field × `Value::Number` → `Real` directly (skip parse).
/// - `Checkbox` field × `Value::Bool` → `Integer(0|1)` directly (skip
///   `"on"`/`"true"` string match).
///
/// For all other combinations falls through to stringify + `coerce_value`,
/// which holds the canonical per-field-type semantics (empty-string ⇒ Null,
/// date normalization, checkbox truthy-string match, number parse). This
/// keeps cross-type coercion correct: e.g. `Bool(true)` to a `Text` field
/// becomes `Text("true")`, not `Integer(1)`.
pub(crate) fn coerce_json_value(field_type: &FieldType, val: &Value) -> DbValue {
    match (field_type, val) {
        (FieldType::Number, Value::Number(n)) => return DbValue::Real(n.as_f64().unwrap_or(0.0)),
        (FieldType::Checkbox, Value::Bool(b)) => return DbValue::Integer(i64::from(*b)),
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

/// Canonicalize a **scalar has-many** field's value into the JSON-array TEXT
/// stored in its column (see [`FieldDefinition::is_has_many_scalar`]).
///
/// Accepts the two shapes that reach the write edge — a typed `Value::Array`
/// (gRPC / Lua / MCP) and a JSON-array *string* (admin form, pre-normalized by
/// `transform_select_has_many`) — and maps every element to the field's own type
/// so the stored element types are identical regardless of ingress surface:
/// `Number` → JSON number (whole values as integers, non-numeric elements
/// dropped), every other scalar (`Text` / `Select` / `Radio`) → string. A null
/// value stores SQL `NULL`; any present value stores a JSON array (`[]` when
/// empty). The read path reverses this via [`parse_has_many_scalar`].
///
/// [`FieldDefinition::is_has_many_scalar`]: crate::core::FieldDefinition::is_has_many_scalar
pub(crate) fn coerce_has_many_scalar(field_type: &FieldType, val: &Value) -> DbValue {
    if val.is_null() {
        return DbValue::Null;
    }

    let elements = match val {
        Value::Array(arr) => arr.clone(),
        Value::String(s) => serde_json::from_str::<Vec<Value>>(s).unwrap_or_default(),
        _ => Vec::new(),
    };

    let canonical: Vec<Value> = elements
        .iter()
        .filter_map(|el| canonical_has_many_element(field_type, el))
        .collect();

    DbValue::Text(Value::Array(canonical).to_string())
}

/// Map one has-many element to the field's canonical JSON type, or `None` to
/// drop it (a non-numeric element in a `Number` list, or a null), mirroring the
/// single-value coercion's "invalid ⇒ dropped" rule.
fn canonical_has_many_element(field_type: &FieldType, el: &Value) -> Option<Value> {
    if el.is_null() {
        return None;
    }

    if *field_type == FieldType::Number {
        let n = match el {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        }?;

        return n.is_finite().then(|| real_to_json_number(n));
    }

    // Text / Select / Radio → string form.
    let s = match el {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };

    Some(Value::String(s))
}

/// Parse a **scalar has-many** column's stored TEXT back into a typed JSON array
/// on read — the inverse of [`coerce_has_many_scalar`]. `row_to_document` is
/// type-blind and yields the raw string, so every read surface would otherwise
/// see `"[1,2]"` instead of `[1, 2]`. A SQL `NULL` (absent value) stays `Null`;
/// a malformed value falls back to an empty array so a reader never sees the raw
/// string.
#[must_use]
pub(crate) fn parse_has_many_scalar(field_type: &FieldType, val: &Value) -> Value {
    let raw = match val {
        Value::Null => return Value::Null,
        Value::Array(_) => return val.clone(),
        Value::String(s) => s,
        _ => return Value::Array(Vec::new()),
    };

    let Ok(elements) = serde_json::from_str::<Vec<Value>>(raw) else {
        return Value::Array(Vec::new());
    };

    let canonical = elements
        .iter()
        .filter_map(|el| canonical_has_many_element(field_type, el))
        .collect();

    Value::Array(canonical)
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

// The field-tree walkers `prefixed_name` and `walk_leaf_fields` now live in
// `core::walk` (the single home for every field-tree traversal). Re-exported
// here so the many `query::helpers::{prefixed_name, walk_leaf_fields}` call
// sites keep their import path.
pub(crate) use crate::core::walk::{prefixed_name, walk_leaf_fields};

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

/// Suffix of a Date field's timezone companion column. The single source of
/// truth shared by column generation ([`tz_column`]) and field-name reservation
/// (`parse::fields` rejects user fields ending in this) so the two can't drift.
pub(crate) const TZ_SUFFIX: &str = "_tz";

/// Suffix of a Code field's language companion column. Single source of truth
/// shared by [`lang_column`] and field-name reservation — see [`TZ_SUFFIX`].
pub(crate) const LANG_SUFFIX: &str = "_lang";

/// Build a timezone companion column name: `"field_tz"`, `"seo__start_tz"`.
pub(crate) fn tz_column(name: &str) -> String {
    format!("{name}{TZ_SUFFIX}")
}

/// Build a code-language companion column name: `"snippet_lang"`,
/// `"meta__example_lang"`. Used by code fields with a non-empty
/// `admin.languages` allow-list — see `apply_code` in the field-context
/// builder.
pub(crate) fn lang_column(name: &str) -> String {
    format!("{name}{LANG_SUFFIX}")
}

/// Build a join table name: `"collection_field"`, `"posts_tags"`.
pub(crate) fn join_table(collection: &str, field: &str) -> String {
    format!("{collection}_{field}")
}

/// Build the table name for a global: `"_global_{slug}"`.
pub(crate) fn global_table(slug: &str) -> String {
    format!("_global_{slug}")
}

/// Quote a SQL identifier (column/table name) for interpolation into DDL/DML.
///
/// Both `SQLite` and Postgres delimit identifiers with double quotes; an embedded
/// `"` is doubled per the SQL standard. Applied at every identifier-emission site
/// so a column whose name is a SQL reserved word — a user field legitimately
/// named `order`, `select`, `group`, … (allowed by field-name validation) — is
/// valid on every backend. (`SQLite`'s legacy "double-quoted string literal"
/// misfeature, which would otherwise turn a quoted *missing* column into a silent
/// string literal, is disabled per-connection via `SQLITE_DBCONFIG_DQS_*` — see
/// the pool setup — so a quoted identifier is always an identifier.)
#[must_use]
pub(crate) fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
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

    // ── scalar has-many coercion (write) + parse (read) ─────────────────

    fn stored(field_type: &FieldType, v: &Value) -> String {
        match coerce_has_many_scalar(field_type, v) {
            DbValue::Text(s) => s,
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn has_many_number_typed_array_stays_numeric() {
        assert_eq!(stored(&FieldType::Number, &json!([1, 2, 3])), "[1,2,3]");
    }

    /// The admin form pre-normalizes to a JSON array of *strings*; a Number list
    /// must be re-typed to numbers so it matches the API-written shape.
    #[test]
    fn has_many_number_stringified_elements_become_numbers() {
        assert_eq!(stored(&FieldType::Number, &json!(["1", "2"])), "[1,2]");
    }

    /// Admin sends the whole value as a JSON-array *string*.
    #[test]
    fn has_many_number_json_string_input_is_parsed() {
        assert_eq!(stored(&FieldType::Number, &json!("[1, 2]")), "[1,2]");
    }

    #[test]
    fn has_many_number_whole_floats_serialize_as_integers() {
        assert_eq!(stored(&FieldType::Number, &json!([1.0, 2.5])), "[1,2.5]");
    }

    #[test]
    fn has_many_number_drops_non_numeric_elements() {
        assert_eq!(
            stored(&FieldType::Number, &json!([1, "x", null, 3])),
            "[1,3]"
        );
    }

    #[test]
    fn has_many_text_numbers_become_strings() {
        assert_eq!(stored(&FieldType::Text, &json!([1, 2])), r#"["1","2"]"#);
    }

    #[test]
    fn has_many_text_strings_stay_strings() {
        assert_eq!(
            stored(&FieldType::Select, &json!(["a", "b"])),
            r#"["a","b"]"#
        );
    }

    #[test]
    fn has_many_null_stores_sql_null() {
        assert_eq!(
            coerce_has_many_scalar(&FieldType::Number, &Value::Null),
            DbValue::Null
        );
    }

    #[test]
    fn has_many_empty_array_stores_empty_json() {
        assert_eq!(stored(&FieldType::Number, &json!([])), "[]");
    }

    #[test]
    fn parse_has_many_number_string_to_typed_array() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Number, &json!("[1,2,3]")),
            json!([1, 2, 3])
        );
    }

    #[test]
    fn parse_has_many_text_string_to_typed_array() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Text, &json!(r#"["a","b"]"#)),
            json!(["a", "b"])
        );
    }

    #[test]
    fn parse_has_many_null_stays_null() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Number, &Value::Null),
            Value::Null
        );
    }

    #[test]
    fn parse_has_many_array_passes_through() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Number, &json!([1, 2])),
            json!([1, 2])
        );
    }

    #[test]
    fn parse_has_many_malformed_falls_back_to_empty_array() {
        assert_eq!(
            parse_has_many_scalar(&FieldType::Number, &json!("not json")),
            json!([])
        );
    }

    /// Write then read round-trips to a typed array, agreeing regardless of the
    /// input shape (typed vs admin-stringified).
    #[test]
    fn has_many_write_read_round_trip_agrees_across_shapes() {
        for input in [json!([1, 2]), json!(["1", "2"]), json!("[1,2]")] {
            let DbValue::Text(stored) = coerce_has_many_scalar(&FieldType::Number, &input) else {
                panic!("expected Text");
            };
            let read = parse_has_many_scalar(&FieldType::Number, &Value::String(stored));
            assert_eq!(read, json!([1, 2]), "input {input:?}");
        }
    }
}
