//! Feature configuration: email, depth, pagination, uploads, locale, jobs, live, hooks, access, MCP.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use super::parsing::{serde_duration, serde_duration_option, serde_filesize};

/// SMTP TLS mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum SmtpTls {
    /// Connect plain, upgrade via STARTTLS (port 587).
    #[default]
    Starttls,
    /// Implicit TLS from the start (port 465).
    Tls,
    /// No encryption (local/test servers, port 25/1025).
    None,
}

/// SMTP email configuration. Empty `smtp_host` disables email (no-op sends).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct EmailConfig {
    /// SMTP server hostname. Empty = email disabled.
    pub smtp_host: String,
    /// SMTP server port (default 587).
    pub smtp_port: u16,
    /// SMTP username for authentication.
    pub smtp_user: String,
    /// SMTP password for authentication.
    pub smtp_pass: String,
    /// "From" email address (default "noreply@example.com").
    pub from_address: String,
    /// "From" display name (default "Crap CMS").
    pub from_name: String,
    /// TLS mode: "starttls" (default), "tls" (implicit), "none" (plain).
    pub smtp_tls: SmtpTls,
    /// SMTP connection/send timeout in seconds (default 30).
    #[serde(default = "default_smtp_timeout", with = "serde_duration")]
    pub smtp_timeout: u64,
}

fn default_smtp_timeout() -> u64 {
    30
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_user: String::new(),
            smtp_pass: String::new(),
            from_address: "noreply@example.com".to_string(),
            from_name: "Crap CMS".to_string(),
            smtp_tls: SmtpTls::default(),
            smtp_timeout: 30,
        }
    }
}

/// Controls relationship population depth defaults and limits.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DepthConfig {
    /// Default population depth when request doesn't specify one.
    /// Used as default for FindByID. Find defaults to 0 regardless.
    pub default_depth: i32,
    /// Maximum allowed depth application-wide. Prevents abuse.
    pub max_depth: i32,
    /// Enable cross-request populate cache for relationship population.
    /// Caches populated documents across requests, cleared on any write
    /// through the API. Opt-in because external DB mutations can cause
    /// stale reads. Default: false.
    #[serde(default)]
    pub populate_cache: bool,
    /// Max age in seconds for the populate cache (periodic full clear).
    /// 0 = no periodic clearing (only write-through invalidation).
    /// Set > 0 to handle external DB mutations. Only used when
    /// `populate_cache` is true.
    #[serde(default)]
    pub populate_cache_max_age_secs: u64,
}

impl Default for DepthConfig {
    fn default() -> Self {
        Self {
            default_depth: 1,
            max_depth: 10,
            populate_cache: false,
            populate_cache_max_age_secs: 0,
        }
    }
}

/// Controls default and maximum page sizes, and pagination mode.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PaginationConfig {
    /// Default page size when request doesn't specify a limit.
    pub default_limit: i64,
    /// Maximum allowed limit. Requests above this are clamped.
    pub max_limit: i64,
    /// Pagination mode: `"page"` (offset-based, default) or `"cursor"` (keyset-based).
    pub mode: PaginationMode,
}

impl Default for PaginationConfig {
    fn default() -> Self {
        Self {
            default_limit: 20,
            max_limit: 1000,
            mode: PaginationMode::Page,
        }
    }
}

impl PaginationConfig {
    /// Whether cursor-based pagination is active.
    pub fn is_cursor(&self) -> bool {
        matches!(self.mode, PaginationMode::Cursor)
    }
}

/// Pagination strategy.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PaginationMode {
    /// Offset-based pagination (page numbers).
    Page,
    /// Keyset-based pagination (cursors).
    Cursor,
}

/// MCP (Model Context Protocol) server configuration.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct McpConfig {
    /// Enable MCP server (default: false).
    pub enabled: bool,
    /// Enable HTTP transport on /mcp (default: false).
    pub http: bool,
    /// Enable config generation tools that can write files to disk (default: false).
    pub config_tools: bool,
    /// API key for HTTP transport auth (empty = no auth).
    pub api_key: String,
    /// Whitelist of collection slugs to expose (empty = all).
    pub include_collections: Vec<String>,
    /// Blacklist of collection slugs to hide (takes precedence over include).
    pub exclude_collections: Vec<String>,
}

/// Global upload settings (per-collection upload config is separate).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct UploadConfig {
    /// Global max file size in bytes. Default: 50MB.
    /// Accepts integer bytes or human-readable string ("50MB", "1GB").
    #[serde(with = "serde_filesize")]
    pub max_file_size: u64,
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            max_file_size: 52_428_800, // 50MB
        }
    }
}

/// Internationalization / locale configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LocaleConfig {
    /// Default locale code. Content without explicit locale uses this.
    pub default_locale: String,
    /// All supported locale codes. Empty = localization disabled.
    pub locales: Vec<String>,
    /// When true, reading a locale falls back to default_locale if the field is NULL.
    pub fallback: bool,
}

impl Default for LocaleConfig {
    fn default() -> Self {
        Self {
            default_locale: "en".to_string(),
            locales: Vec::new(),
            fallback: true,
        }
    }
}

impl LocaleConfig {
    /// Returns true if localization is enabled (at least one locale defined).
    pub fn is_enabled(&self) -> bool {
        !self.locales.is_empty()
    }

    /// Validate that all locale codes are safe identifiers (alphanumeric, hyphens,
    /// underscores only). This prevents SQL injection via locale strings that are
    /// interpolated into DDL during migrations.
    pub fn validate(&self) -> Result<()> {
        Self::validate_locale_code(&self.default_locale)?;
        for locale in &self.locales {
            Self::validate_locale_code(locale)?;
        }
        Ok(())
    }

    fn validate_locale_code(code: &str) -> Result<()> {
        if code.is_empty() {
            bail!("Locale code must not be empty");
        }
        if !code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!(
                "Invalid locale code '{}': only ASCII alphanumeric, hyphens, and underscores allowed",
                code
            );
        }
        Ok(())
    }
}

/// Background job scheduler configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct JobsConfig {
    /// Max concurrent job executions across all queues. Default: 10.
    pub max_concurrent: usize,
    /// How often to poll for pending jobs, in seconds. Default: 1s.
    /// Accepts integer seconds or human-readable string ("1s", "5s").
    #[serde(with = "serde_duration")]
    pub poll_interval: u64,
    /// How often to check cron schedules, in seconds. Default: 60s.
    #[serde(with = "serde_duration")]
    pub cron_interval: u64,
    /// How often to update heartbeat for running jobs, in seconds. Default: 10s.
    #[serde(with = "serde_duration")]
    pub heartbeat_interval: u64,
    /// Auto-purge completed/failed jobs older than this duration (in seconds).
    /// Accepts integer seconds or human-readable string ("7d", "24h").
    /// None disables auto-purge.
    #[serde(with = "serde_duration_option")]
    pub auto_purge: Option<u64>,
    /// Number of pending image conversions to process per scheduler poll. Default: 10.
    pub image_queue_batch_size: usize,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 10,
            poll_interval: 1,
            cron_interval: 60,
            heartbeat_interval: 10,
            auto_purge: Some(7 * 86400), // 7 days
            image_queue_batch_size: 10,
        }
    }
}

/// Live event streaming configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LiveConfig {
    /// Enable live event streaming (SSE + gRPC Subscribe). Default: true.
    pub enabled: bool,
    /// Broadcast channel capacity. Default: 1024.
    pub channel_capacity: usize,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            channel_capacity: 1024,
        }
    }
}

/// Hook configuration — `on_init` script references and recursion limits.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct HooksConfig {
    /// List of Lua script names (without extension) to run once when the CMS starts up.
    /// These are loaded from the `hooks/` directory.
    pub on_init: Vec<String>,
    /// Max hook recursion depth for Lua CRUD → hook → CRUD chains.
    /// 0 = disable hooks from Lua CRUD entirely. Default: 3.
    pub max_depth: u32,
    /// Number of Lua VMs in the hook runner pool. Default: max(available_parallelism, 4), capped at 32.
    /// Higher values allow more concurrent hook execution.
    pub vm_pool_size: usize,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            on_init: Vec::new(),
            max_depth: 3,
            vm_pool_size: std::thread::available_parallelism()
                .map(|n| n.get().clamp(4, 32))
                .unwrap_or(8),
        }
    }
}

/// Access control defaults.
/// When `default_deny` is true, collections/globals without explicit access functions
/// deny all operations instead of allowing them. Default: false (backward compatible).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct AccessConfig {
    /// When true, operations on collections/globals without an explicit access function
    /// are denied by default. When false (default), missing access functions allow all.
    pub default_deny: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_config_defaults() {
        let email = EmailConfig::default();
        assert!(
            email.smtp_host.is_empty(),
            "smtp_host should be empty by default"
        );
        assert_eq!(email.smtp_port, 587);
        assert!(email.smtp_user.is_empty());
        assert!(email.smtp_pass.is_empty());
        assert_eq!(email.smtp_tls, SmtpTls::Starttls);
        assert_eq!(email.from_address, "noreply@example.com");
        assert_eq!(email.from_name, "Crap CMS");
    }

    #[test]
    fn depth_config_defaults() {
        let depth = DepthConfig::default();
        assert_eq!(depth.default_depth, 1);
        assert_eq!(depth.max_depth, 10);
    }

    #[test]
    fn auto_purge_default_config() {
        let cfg = JobsConfig::default();
        assert_eq!(cfg.auto_purge, Some(7 * 86400));
    }

    #[test]
    fn jobs_config_defaults() {
        let cfg = JobsConfig::default();
        assert_eq!(cfg.max_concurrent, 10);
        assert_eq!(cfg.poll_interval, 1);
        assert_eq!(cfg.cron_interval, 60);
        assert_eq!(cfg.heartbeat_interval, 10);
        assert_eq!(cfg.auto_purge, Some(7 * 86400));
        assert_eq!(cfg.image_queue_batch_size, 10);
    }

    #[test]
    fn jobs_image_queue_batch_size_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs]\nimage_queue_batch_size = 50\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.image_queue_batch_size, 50);
    }

    #[test]
    fn upload_config_defaults() {
        let upload = UploadConfig::default();
        assert_eq!(upload.max_file_size, 52_428_800);
    }

    #[test]
    fn live_config_defaults() {
        let live = LiveConfig::default();
        assert!(live.enabled);
        assert_eq!(live.channel_capacity, 1024);
    }

    #[test]
    fn hooks_config_defaults() {
        let hooks = HooksConfig::default();
        assert!(hooks.on_init.is_empty());
        assert_eq!(hooks.max_depth, 3);
        assert!(hooks.vm_pool_size >= 4 && hooks.vm_pool_size <= 32);
    }

    #[test]
    fn locale_config_is_enabled() {
        let empty = LocaleConfig::default();
        assert!(!empty.is_enabled(), "empty locales should be disabled");

        let with_locales = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        assert!(
            with_locales.is_enabled(),
            "non-empty locales should be enabled"
        );
    }

    #[test]
    fn locale_validation_valid_codes() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec![
                "en".to_string(),
                "de".to_string(),
                "pt-BR".to_string(),
                "zh_CN".to_string(),
            ],
            fallback: true,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn locale_validation_rejects_sql_injection() {
        let config = LocaleConfig {
            default_locale: "en'; DROP TABLE posts; --".to_string(),
            locales: vec![],
            fallback: true,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn locale_validation_rejects_empty() {
        let config = LocaleConfig {
            default_locale: "".to_string(),
            locales: vec![],
            fallback: true,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn locale_validation_rejects_bad_locale_in_list() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de/../etc".to_string()],
            fallback: true,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn access_config_default_deny_false_by_default() {
        let config = crate::config::CrapConfig::default();
        assert!(!config.access.default_deny);
    }

    #[test]
    fn access_config_default_deny_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[access]\ndefault_deny = true\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert!(config.access.default_deny);
    }
}
