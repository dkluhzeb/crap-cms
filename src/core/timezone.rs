//! Common IANA timezone constants for the admin UI.

/// Common IANA timezone options for the admin UI dropdown.
/// Each entry is (IANA code, human-readable label).
pub const TIMEZONE_OPTIONS: &[(&str, &str)] = &[
    // UTC
    ("UTC", "UTC"),
    // Americas
    ("America/New_York", "Eastern Time (US & Canada)"),
    ("America/Chicago", "Central Time (US & Canada)"),
    ("America/Denver", "Mountain Time (US & Canada)"),
    ("America/Los_Angeles", "Pacific Time (US & Canada)"),
    ("America/Anchorage", "Alaska"),
    ("Pacific/Honolulu", "Hawaii"),
    ("America/Phoenix", "Arizona (no DST)"),
    ("America/Toronto", "Eastern Time (Canada)"),
    ("America/Vancouver", "Pacific Time (Canada)"),
    ("America/Winnipeg", "Central Time (Canada)"),
    ("America/Halifax", "Atlantic Time (Canada)"),
    ("America/St_Johns", "Newfoundland"),
    ("America/Mexico_City", "Mexico City"),
    ("America/Bogota", "Bogota"),
    ("America/Lima", "Lima"),
    ("America/Santiago", "Santiago"),
    ("America/Buenos_Aires", "Buenos Aires"),
    ("America/Sao_Paulo", "Sao Paulo"),
    // Europe
    ("Europe/London", "London"),
    ("Europe/Dublin", "Dublin"),
    ("Europe/Paris", "Paris"),
    ("Europe/Berlin", "Berlin"),
    ("Europe/Amsterdam", "Amsterdam"),
    ("Europe/Brussels", "Brussels"),
    ("Europe/Zurich", "Zurich"),
    ("Europe/Vienna", "Vienna"),
    ("Europe/Rome", "Rome"),
    ("Europe/Madrid", "Madrid"),
    ("Europe/Lisbon", "Lisbon"),
    ("Europe/Stockholm", "Stockholm"),
    ("Europe/Oslo", "Oslo"),
    ("Europe/Copenhagen", "Copenhagen"),
    ("Europe/Helsinki", "Helsinki"),
    ("Europe/Warsaw", "Warsaw"),
    ("Europe/Prague", "Prague"),
    ("Europe/Budapest", "Budapest"),
    ("Europe/Bucharest", "Bucharest"),
    ("Europe/Athens", "Athens"),
    ("Europe/Istanbul", "Istanbul"),
    ("Europe/Moscow", "Moscow"),
    ("Europe/Kyiv", "Kyiv"),
    // Asia
    ("Asia/Dubai", "Dubai"),
    ("Asia/Riyadh", "Riyadh"),
    ("Asia/Tehran", "Tehran"),
    ("Asia/Karachi", "Karachi"),
    ("Asia/Kolkata", "Mumbai / Kolkata"),
    ("Asia/Dhaka", "Dhaka"),
    ("Asia/Bangkok", "Bangkok"),
    ("Asia/Jakarta", "Jakarta"),
    ("Asia/Singapore", "Singapore"),
    ("Asia/Hong_Kong", "Hong Kong"),
    ("Asia/Shanghai", "Shanghai"),
    ("Asia/Taipei", "Taipei"),
    ("Asia/Seoul", "Seoul"),
    ("Asia/Tokyo", "Tokyo"),
    // Oceania
    ("Australia/Perth", "Perth"),
    ("Australia/Adelaide", "Adelaide"),
    ("Australia/Sydney", "Sydney"),
    ("Australia/Brisbane", "Brisbane"),
    ("Pacific/Auckland", "Auckland"),
    ("Pacific/Fiji", "Fiji"),
    // Africa
    ("Africa/Cairo", "Cairo"),
    ("Africa/Lagos", "Lagos"),
    ("Africa/Nairobi", "Nairobi"),
    ("Africa/Johannesburg", "Johannesburg"),
    ("Africa/Casablanca", "Casablanca"),
];

#[cfg(test)]
mod tests {
    use chrono_tz::Tz;

    use super::*;

    #[test]
    fn all_timezone_options_are_valid_iana() {
        for (tz_str, label) in TIMEZONE_OPTIONS {
            assert!(
                tz_str.parse::<Tz>().is_ok(),
                "Invalid IANA timezone '{}' (label: '{}')",
                tz_str,
                label
            );
        }
    }

    #[test]
    fn no_duplicate_timezone_codes() {
        let mut seen = std::collections::HashSet::new();

        for (tz_str, _) in TIMEZONE_OPTIONS {
            assert!(seen.insert(tz_str), "Duplicate timezone code: {}", tz_str);
        }
    }
}
