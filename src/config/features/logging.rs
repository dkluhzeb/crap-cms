//! File-based logging configuration with rotation.

use serde::{Deserialize, Serialize};

/// Log rotation strategy for file-based logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogRotation {
    /// Rotate log files every hour.
    Hourly,
    /// Rotate log files every day (default).
    #[default]
    Daily,
    /// Never rotate -- single log file that grows indefinitely.
    Never,
}

/// File-based logging configuration.
///
/// When `file` is true, logs are written to rotating files in `path` (relative to
/// the config directory, or an absolute path). Disabled by default -- stdout-only
/// logging is the default for backward compatibility and Docker deployments.
#[derive(Debug, Clone, Deserialize, Serialize, crap_cms_macros::ConfigKeys)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    /// Enable file logging. Default: false.
    pub file: bool,
    /// Log directory path (relative to config dir, or absolute). Default: "data/logs".
    pub path: String,
    /// Log rotation strategy: "hourly", "daily", or "never". Default: "daily".
    pub rotation: LogRotation,
    /// Maximum number of rotated log files to keep. Default: 30.
    /// Old files are pruned on startup.
    pub max_files: usize,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            file: false,
            path: "data/logs".to_string(),
            rotation: LogRotation::default(),
            max_files: 30,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_config_defaults() {
        let logging = LoggingConfig::default();
        assert!(!logging.file);
        assert_eq!(logging.path, "data/logs");
        assert_eq!(logging.rotation, LogRotation::Daily);
        assert_eq!(logging.max_files, 30);
    }

    #[test]
    fn logging_config_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[logging]\nfile = true\npath = \"logs\"\nrotation = \"hourly\"\nmax_files = 7\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert!(config.logging.file);
        assert_eq!(config.logging.path, "logs");
        assert_eq!(config.logging.rotation, LogRotation::Hourly);
        assert_eq!(config.logging.max_files, 7);
    }

    #[test]
    fn logging_config_partial_toml_uses_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("crap.toml"), "[logging]\nfile = true\n").unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert!(config.logging.file);
        assert_eq!(config.logging.path, "data/logs");
        assert_eq!(config.logging.rotation, LogRotation::Daily);
        assert_eq!(config.logging.max_files, 30);
    }

    #[test]
    fn logging_rotation_never_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[logging]\nfile = true\nrotation = \"never\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.logging.rotation, LogRotation::Never);
    }
}
