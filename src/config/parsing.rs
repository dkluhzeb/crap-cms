//! Duration and file-size parsing, plus serde helpers for human-readable values.

/// Parse a human-readable duration string into seconds.
///
/// Supports: `"30s"` (seconds), `"30m"` (minutes), `"24h"` (hours), `"7d"` (days).
/// Returns `None` for empty or invalid input.
pub(crate) fn parse_duration_string(s: &str) -> Option<u64> {
    let s = s.trim();

    if s.is_empty() {
        return None;
    }

    // Bare number (no suffix) treated as seconds
    if let Ok(secs) = s.parse::<u64>() {
        return Some(secs);
    }

    let suffix = s.chars().last()?;
    let num_str = &s[..s.len() - suffix.len_utf8()];
    let num: u64 = num_str.parse().ok()?;

    match suffix {
        's' => Some(num),
        'm' => Some(num * 60),
        'h' => Some(num * 3600),
        'd' => Some(num * 86400),
        _ => None,
    }
}

/// Parse a human-readable file size string into bytes.
///
/// Supports: `"500B"` (bytes), `"100KB"` (kilobytes), `"50MB"` (megabytes), `"1GB"` (gigabytes).
/// Uses 1024-based (binary) units. Case-insensitive.
/// Returns `None` for empty or invalid input.
pub(crate) fn parse_filesize_string(s: &str) -> Option<u64> {
    let s = s.trim();

    if s.is_empty() {
        return None;
    }

    // Only ASCII characters are valid in file size strings
    if !s.is_ascii() {
        return None;
    }

    let upper = s.to_ascii_uppercase();

    // Try two-char suffix first (KB, MB, GB), then one-char (B)
    if upper.len() >= 3 {
        let (num_str, suffix) = upper.split_at(upper.len() - 2);
        match suffix {
            "KB" => return num_str.parse::<u64>().ok().map(|n| n * 1024),
            "MB" => return num_str.parse::<u64>().ok().map(|n| n * 1024 * 1024),
            "GB" => return num_str.parse::<u64>().ok().map(|n| n * 1024 * 1024 * 1024),
            _ => {}
        }
    }

    if upper.ends_with('B') {
        let num_str = &upper[..upper.len() - 1];

        return num_str.parse::<u64>().ok();
    }

    None
}

/// Serde deserializer that accepts both an integer (seconds) and a human-readable
/// duration string (`"30s"`, `"5m"`, `"2h"`, `"7d"`). Used for config fields where
/// backward compatibility with plain integer seconds is desired.
pub(crate) mod serde_duration {
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum DurationValue {
            Seconds(u64),
            Human(String),
        }

        match DurationValue::deserialize(deserializer)? {
            DurationValue::Seconds(s) => Ok(s),
            DurationValue::Human(s) => {
                super::parse_duration_string(&s).ok_or_else(|| {
                    serde::de::Error::custom(format!(
                        "invalid duration '{}': use an integer (seconds) or a string like \"30s\", \"5m\", \"2h\", \"7d\"",
                        s
                    ))
                })
            }
        }
    }

    pub fn serialize<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(*value)
    }
}

/// Serde deserializer for optional duration fields. Absent/null → None,
/// integer (seconds) or human string → Some(seconds).
pub(crate) mod serde_duration_option {
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum DurationValue {
            Seconds(u64),
            Human(String),
        }

        let opt: Option<DurationValue> = Option::deserialize(deserializer)?;
        match opt {
            None => Ok(None),
            Some(DurationValue::Seconds(s)) => Ok(Some(s)),
            Some(DurationValue::Human(s)) => {
                if s.is_empty() {
                    return Ok(None);
                }
                super::parse_duration_string(&s).map(Some).ok_or_else(|| {
                    serde::de::Error::custom(format!(
                        "invalid duration '{}': use an integer (seconds) or a string like \"30s\", \"5m\", \"2h\", \"7d\"",
                        s
                    ))
                })
            }
        }
    }

    pub fn serialize<S>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(v) => serializer.serialize_u64(*v),
            None => serializer.serialize_none(),
        }
    }
}

/// Serde helper for duration fields stored in milliseconds. Accepts either a raw
/// integer (milliseconds, backward compatible) or a human-readable duration string
/// (`"30s"`, `"5m"`, `"2h"`) which is converted to milliseconds.
pub(crate) mod serde_duration_ms {
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum DurationValue {
            Millis(u64),
            Human(String),
        }

        match DurationValue::deserialize(deserializer)? {
            DurationValue::Millis(ms) => Ok(ms),
            DurationValue::Human(s) => {
                let secs = super::parse_duration_string(&s).ok_or_else(|| {
                    serde::de::Error::custom(format!(
                        "invalid duration '{}': use an integer (milliseconds) or a string like \"30s\", \"5m\", \"2h\"",
                        s
                    ))
                })?;
                Ok(secs * 1000)
            }
        }
    }

    pub fn serialize<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(*value)
    }
}

/// Serde deserializer that accepts both an integer (bytes) and a human-readable
/// file size string (`"500B"`, `"100KB"`, `"50MB"`, `"1GB"`). Used for config fields.
pub(crate) mod serde_filesize {
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum FilesizeValue {
            Bytes(u64),
            Human(String),
        }

        match FilesizeValue::deserialize(deserializer)? {
            FilesizeValue::Bytes(b) => Ok(b),
            FilesizeValue::Human(s) => {
                super::parse_filesize_string(&s).ok_or_else(|| {
                    serde::de::Error::custom(format!(
                        "invalid file size '{}': use an integer (bytes) or a string like \"500B\", \"100KB\", \"50MB\", \"1GB\"",
                        s
                    ))
                })
            }
        }
    }

    pub fn serialize<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(*value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_duration_string tests --

    #[test]
    fn parse_duration_days() {
        assert_eq!(parse_duration_string("7d"), Some(7 * 86400));
        assert_eq!(parse_duration_string("1d"), Some(86400));
        assert_eq!(parse_duration_string("30d"), Some(30 * 86400));
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration_string("24h"), Some(24 * 3600));
        assert_eq!(parse_duration_string("1h"), Some(3600));
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration_string("30m"), Some(30 * 60));
        assert_eq!(parse_duration_string("1m"), Some(60));
    }

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration_string("30s"), Some(30));
        assert_eq!(parse_duration_string("1s"), Some(1));
    }

    #[test]
    fn parse_duration_invalid() {
        assert_eq!(parse_duration_string(""), None);
        assert_eq!(parse_duration_string("abc"), None);
        assert_eq!(parse_duration_string("7x"), None);
        assert_eq!(parse_duration_string("d"), None);
    }

    #[test]
    fn parse_duration_non_ascii_no_panic() {
        // Multi-byte UTF-8 suffixes must not cause a panic from slicing
        assert_eq!(parse_duration_string("5ü"), None);
        assert_eq!(parse_duration_string("5🔥"), None);
        assert_eq!(parse_duration_string("10日"), None);
        assert_eq!(parse_duration_string("ü"), None);
    }

    #[test]
    fn parse_duration_whitespace() {
        assert_eq!(parse_duration_string("  7d  "), Some(7 * 86400));
    }

    // -- parse_filesize_string tests --

    #[test]
    fn parse_filesize_bytes() {
        assert_eq!(parse_filesize_string("500B"), Some(500));
        assert_eq!(parse_filesize_string("0B"), Some(0));
        assert_eq!(parse_filesize_string("1B"), Some(1));
    }

    #[test]
    fn parse_filesize_kilobytes() {
        assert_eq!(parse_filesize_string("100KB"), Some(100 * 1024));
        assert_eq!(parse_filesize_string("1KB"), Some(1024));
    }

    #[test]
    fn parse_filesize_megabytes() {
        assert_eq!(parse_filesize_string("50MB"), Some(50 * 1024 * 1024));
        assert_eq!(parse_filesize_string("1MB"), Some(1024 * 1024));
        assert_eq!(parse_filesize_string("100MB"), Some(100 * 1024 * 1024));
    }

    #[test]
    fn parse_filesize_gigabytes() {
        assert_eq!(parse_filesize_string("1GB"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_filesize_string("2GB"), Some(2 * 1024 * 1024 * 1024));
    }

    #[test]
    fn parse_filesize_case_insensitive() {
        assert_eq!(parse_filesize_string("50mb"), Some(50 * 1024 * 1024));
        assert_eq!(parse_filesize_string("50Mb"), Some(50 * 1024 * 1024));
        assert_eq!(parse_filesize_string("1gb"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_filesize_string("100kb"), Some(100 * 1024));
    }

    #[test]
    fn parse_filesize_whitespace() {
        assert_eq!(parse_filesize_string("  50MB  "), Some(50 * 1024 * 1024));
    }

    #[test]
    fn parse_filesize_invalid() {
        assert_eq!(parse_filesize_string(""), None);
        assert_eq!(parse_filesize_string("abc"), None);
        assert_eq!(parse_filesize_string("50"), None);
        assert_eq!(parse_filesize_string("MB"), None);
        assert_eq!(parse_filesize_string("50TB"), None);
    }

    #[test]
    fn parse_filesize_non_ascii_no_panic() {
        // Non-ASCII input must return None without panicking
        assert_eq!(parse_filesize_string("50MÜ"), None);
        assert_eq!(parse_filesize_string("100🔥B"), None);
        assert_eq!(parse_filesize_string("ü"), None);
        assert_eq!(parse_filesize_string("1日GB"), None);
    }

    // -- serde integration tests (load from TOML) --

    #[test]
    fn serde_filesize_integer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[upload]\nmax_file_size = 52428800\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.upload.max_file_size, 52_428_800);
    }

    #[test]
    fn serde_filesize_string_megabytes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[upload]\nmax_file_size = \"50MB\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.upload.max_file_size, 50 * 1024 * 1024);
    }

    #[test]
    fn serde_filesize_string_gigabytes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[upload]\nmax_file_size = \"1GB\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.upload.max_file_size, 1024 * 1024 * 1024);
    }

    #[test]
    fn serde_duration_integer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[auth]\ntoken_expiry = 7200\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.auth.token_expiry, 7200);
    }

    #[test]
    fn serde_duration_string_hours() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[auth]\ntoken_expiry = \"2h\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.auth.token_expiry, 7200);
    }

    #[test]
    fn serde_duration_string_minutes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[auth]\nlogin_lockout_seconds = \"5m\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.auth.login_lockout_seconds, 300);
    }

    #[test]
    fn serde_duration_ms_human_string() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[database]\nbusy_timeout = \"30s\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.database.busy_timeout, 30000);
    }

    #[test]
    fn serde_duration_ms_integer_backward_compat() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[database]\nbusy_timeout = 15000\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.database.busy_timeout, 15000);
    }

    #[test]
    fn connection_timeout_human_string() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[database]\nconnection_timeout = \"10s\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.database.connection_timeout, 10);
    }

    #[test]
    fn serde_duration_option_string() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs]\nauto_purge = \"7d\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.auto_purge, Some(7 * 86400));
    }

    #[test]
    fn serde_duration_option_integer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("crap.toml"), "[jobs]\nauto_purge = 86400\n").unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.auto_purge, Some(86400));
    }

    #[test]
    fn serde_duration_option_absent_uses_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("crap.toml"), "[jobs]\nmax_concurrent = 5\n").unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.auto_purge, Some(7 * 86400)); // default
    }
}
