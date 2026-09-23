//! The `soft_delete_retention` duration grammar.

/// Parse a retention duration string like "30d", "7d", "24h" into seconds.
/// Returns `None` if the string is not a valid duration.
pub(super) fn parse_retention_seconds(s: &str) -> Option<i64> {
    let s = s.trim();

    if let Some(days) = s.strip_suffix('d') {
        days.parse::<i64>().ok().map(|d| d * 86400)
    } else if let Some(hours) = s.strip_suffix('h') {
        hours.parse::<i64>().ok().map(|h| h * 3600)
    } else if let Some(mins) = s.strip_suffix('m') {
        mins.parse::<i64>().ok().map(|m| m * 60)
    } else if let Some(secs) = s.strip_suffix('s') {
        secs.parse::<i64>().ok()
    } else {
        s.parse::<i64>().ok() // raw seconds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_retention_seconds ───────────────────────────────────────────

    #[test]
    fn parse_retention_days() {
        assert_eq!(parse_retention_seconds("30d"), Some(30 * 86400));
        assert_eq!(parse_retention_seconds("7d"), Some(7 * 86400));
        assert_eq!(parse_retention_seconds("1d"), Some(86400));
    }

    #[test]
    fn parse_retention_hours() {
        assert_eq!(parse_retention_seconds("24h"), Some(24 * 3600));
        assert_eq!(parse_retention_seconds("1h"), Some(3600));
    }

    #[test]
    fn parse_retention_minutes() {
        assert_eq!(parse_retention_seconds("30m"), Some(1800));
        assert_eq!(parse_retention_seconds("1m"), Some(60));
    }

    #[test]
    fn parse_retention_seconds_suffix() {
        assert_eq!(parse_retention_seconds("10s"), Some(10));
        assert_eq!(parse_retention_seconds("1s"), Some(1));
        assert_eq!(parse_retention_seconds("0s"), Some(0));
    }

    #[test]
    fn parse_retention_raw_seconds() {
        assert_eq!(parse_retention_seconds("3600"), Some(3600));
        assert_eq!(parse_retention_seconds("86400"), Some(86400));
    }

    #[test]
    fn parse_retention_invalid() {
        assert_eq!(parse_retention_seconds("abc"), None);
        assert_eq!(parse_retention_seconds(""), None);
        assert_eq!(parse_retention_seconds("d"), None);
    }

    #[test]
    fn parse_retention_with_whitespace() {
        assert_eq!(parse_retention_seconds(" 30d "), Some(30 * 86400));
        assert_eq!(parse_retention_seconds(" 3600 "), Some(3600));
    }
}
