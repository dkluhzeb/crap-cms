//! Date normalization to stored UTC form, timezone conversion and the current
//! timestamp.

use anyhow::{Result, anyhow};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

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
pub(crate) fn normalize_date_with_timezone(value: &str, tz_str: &str) -> Result<String> {
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
/// Returns the local datetime as `YYYY-MM-DDTHH:MM:SS`, which a picker cuts to
/// what it shows: `<input type="datetime-local">` takes the 16-char prefix (or
/// the whole value when the seconds matter), `<input type="date">` the 10-char
/// prefix.
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

    Some(local.format("%Y-%m-%dT%H:%M:%S").to_string())
}

/// Current UTC timestamp in ISO 8601 format with milliseconds: `"2024-01-15T14:00:00.000Z"`.
pub(crate) fn utc_now() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

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
        assert_eq!(result.unwrap(), "2026-05-01T09:00:00");
    }

    #[test]
    fn utc_to_local_new_york() {
        // 14:00 UTC = 09:00 EST (January, UTC-5)
        let result = utc_to_local("2024-01-15T14:00:00.000Z", "America/New_York");
        assert_eq!(result.unwrap(), "2024-01-15T09:00:00");
    }

    #[test]
    fn utc_to_local_utc() {
        let result = utc_to_local("2024-01-15T09:00:00.000Z", "UTC");
        assert_eq!(result.unwrap(), "2024-01-15T09:00:00");
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
        assert_eq!(local, "2026-05-01T09:00:00");
    }

    /// The seconds a stored instant carries survive the conversion, so a
    /// picker that shows them re-submits what is stored.
    #[test]
    fn utc_to_local_keeps_the_seconds() {
        let local = utc_to_local("2026-05-01T12:30:45.000Z", "America/Sao_Paulo").unwrap();
        assert_eq!(local, "2026-05-01T09:30:45");
    }

    /// `utc_now` carries the real milliseconds: a hardcoded `.000` let two
    /// writes in the same second store the same `updated_at`, and a later write
    /// could sort before an earlier one written through a millisecond-accurate
    /// path.
    #[test]
    fn utc_now_carries_real_milliseconds() {
        let first = utc_now();
        let parsed = DateTime::parse_from_rfc3339(&first).expect("RFC 3339");
        assert_eq!(first.len(), "2024-01-15T14:00:00.000Z".len());

        let millis = |s: &str| s[20..23].parse::<u32>().unwrap();
        assert_eq!(millis(&first), parsed.timestamp_subsec_millis());

        let later = (0..200)
            .map(|_| {
                thread::sleep(Duration::from_millis(1));
                utc_now()
            })
            .find(|s| millis(s) != 0);
        assert!(later.is_some(), "milliseconds must not be pinned to .000");
    }
}
