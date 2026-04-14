//! Feature configuration: email, depth, pagination, uploads, locale, jobs, live, hooks, access, MCP.

use std::{collections::HashMap, thread};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::{
    McpApiKey, SmtpPassword,
    parsing::{serde_duration, serde_duration_option, serde_filesize},
};

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
#[serde(default, deny_unknown_fields)]
pub struct EmailConfig {
    /// Email provider: `"smtp"` (default), `"webhook"`, `"log"`, or `"custom"`.
    #[serde(default = "default_email_provider")]
    pub provider: String,
    /// SMTP server hostname. Empty = email disabled (falls back to log provider).
    pub smtp_host: String,
    /// SMTP server port (default 587).
    pub smtp_port: u16,
    /// SMTP username for authentication.
    pub smtp_user: String,
    /// SMTP password for authentication.
    pub smtp_pass: SmtpPassword,
    /// "From" email address (default "noreply@example.com").
    pub from_address: String,
    /// "From" display name (default "Crap CMS").
    pub from_name: String,
    /// TLS mode: "starttls" (default), "tls" (implicit), "none" (plain).
    pub smtp_tls: SmtpTls,
    /// SMTP connection/send timeout in seconds (default 30).
    #[serde(default = "default_smtp_timeout", with = "serde_duration")]
    pub smtp_timeout: u64,
    /// Webhook URL for the webhook email provider.
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// Extra HTTP headers for webhook requests (e.g., Authorization).
    #[serde(default)]
    pub webhook_headers: HashMap<String, String>,
    /// Retry count for queued emails via `crap.email.queue()`. Default: 3.
    #[serde(default = "default_queue_retries")]
    pub queue_retries: u32,
    /// Job queue name for queued emails. Default: "email".
    #[serde(default = "default_queue_name")]
    pub queue_name: String,
    /// Per-attempt timeout for queued email jobs in seconds. Default: 30.
    #[serde(default = "default_queue_timeout")]
    pub queue_timeout: u64,
    /// Max concurrent queued email jobs. Default: 5.
    #[serde(default = "default_queue_concurrency")]
    pub queue_concurrency: u32,
}

fn default_queue_retries() -> u32 {
    3
}

fn default_queue_name() -> String {
    "email".to_string()
}

fn default_queue_timeout() -> u64 {
    30
}

fn default_queue_concurrency() -> u32 {
    5
}

fn default_email_provider() -> String {
    "smtp".to_string()
}

fn default_smtp_timeout() -> u64 {
    30
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            provider: default_email_provider(),
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_user: String::new(),
            smtp_pass: SmtpPassword::new(""),
            from_address: "noreply@example.com".to_string(),
            from_name: "Crap CMS".to_string(),
            smtp_tls: SmtpTls::default(),
            smtp_timeout: 30,
            webhook_url: None,
            webhook_headers: HashMap::new(),
            queue_retries: default_queue_retries(),
            queue_name: default_queue_name(),
            queue_timeout: default_queue_timeout(),
            queue_concurrency: default_queue_concurrency(),
        }
    }
}

/// Controls relationship population depth defaults and limits.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct DepthConfig {
    /// Default population depth when request doesn't specify one.
    /// Used as default for FindByID. Find defaults to 0 regardless.
    pub default_depth: i32,
    /// Maximum allowed depth application-wide. Prevents abuse.
    pub max_depth: i32,
}

impl Default for DepthConfig {
    fn default() -> Self {
        Self {
            default_depth: 1,
            max_depth: 10,
        }
    }
}

/// Cache backend configuration.
///
/// Configures the cross-request cache used for relationship population and
/// other cacheable data. The cache is cleared on any write operation.
///
/// ```toml
/// [cache]
/// backend = "memory"      # "memory" (default), "redis", "none", "custom"
/// max_entries = 10000      # soft cap for memory backend
/// max_age_secs = 0         # periodic full clear (0 = disabled)
/// redis_url = "redis://127.0.0.1:6379"
/// prefix = "crap:"         # key prefix for Redis
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CacheConfig {
    /// Cache backend: `"memory"` (default), `"redis"`, `"none"`, or `"custom"`.
    #[serde(default = "default_cache_backend")]
    pub backend: String,
    /// Soft cap on the number of entries for the memory backend.
    /// Once reached, new insertions are skipped until a clear. Default: 10,000.
    #[serde(default = "default_cache_max_entries")]
    pub max_entries: usize,
    /// Periodic full clear interval in seconds. 0 = disabled (only
    /// write-through invalidation). Set > 0 to handle external DB mutations.
    #[serde(default)]
    pub max_age_secs: u64,
    /// Redis connection URL. Only used when `backend = "redis"`.
    #[serde(default = "default_redis_url")]
    pub redis_url: String,
    /// Key prefix for the Redis backend. All keys are stored as `{prefix}{key}`.
    #[serde(default = "default_cache_prefix")]
    pub prefix: String,
}

fn default_cache_backend() -> String {
    "memory".to_string()
}

fn default_cache_max_entries() -> usize {
    10_000
}

fn default_redis_url() -> String {
    "redis://127.0.0.1:6379".to_string()
}

fn default_cache_prefix() -> String {
    "crap:".to_string()
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            backend: default_cache_backend(),
            max_entries: default_cache_max_entries(),
            max_age_secs: 0,
            redis_url: default_redis_url(),
            prefix: default_cache_prefix(),
        }
    }
}

/// Controls default and maximum page sizes, and pagination mode.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    /// Enable MCP server (default: false).
    pub enabled: bool,
    /// Enable HTTP transport on /mcp (default: false).
    pub http: bool,
    /// Enable config generation tools that can write files to disk (default: false).
    pub config_tools: bool,
    /// API key for HTTP transport auth. **Required** when `http = true` — the server
    /// will refuse to start without one. The HTTP handler also rejects all requests
    /// when the API key is empty as a defense-in-depth measure.
    pub api_key: McpApiKey,
    /// Whitelist of collection slugs to expose (empty = all).
    pub include_collections: Vec<String>,
    /// Blacklist of collection slugs to hide (takes precedence over include).
    pub exclude_collections: Vec<String>,
}

/// Global upload settings (per-collection upload config is separate).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UploadConfig {
    /// Storage backend: `"local"` (default), `"s3"`, or `"custom"`.
    #[serde(default = "default_upload_storage")]
    pub storage: String,
    /// Global max file size in bytes. Default: 50MB.
    /// Accepts integer bytes or human-readable string ("50MB", "1GB").
    #[serde(with = "serde_filesize")]
    pub max_file_size: u64,
    /// S3-compatible storage configuration. Only used when `storage = "s3"`.
    #[serde(default)]
    pub s3: S3Config,
}

/// S3-compatible storage configuration.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    /// S3 bucket name.
    #[serde(default)]
    pub bucket: String,
    /// AWS region (e.g., `"us-east-1"`). Default: `"us-east-1"`.
    #[serde(default = "default_s3_region")]
    pub region: String,
    /// S3 endpoint URL. Default: AWS. Set for MinIO, R2, etc.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Access key ID.
    #[serde(default)]
    pub access_key: String,
    /// Secret access key.
    #[serde(default)]
    pub secret_key: String,
    /// Optional key prefix prepended to all storage keys.
    #[serde(default)]
    pub prefix: String,
    /// Base URL for public file URLs (e.g., CDN URL).
    /// If empty, generates S3 URLs.
    #[serde(default)]
    pub public_url_base: String,
    /// Use path-style addressing (required for MinIO and some providers).
    #[serde(default)]
    pub path_style: bool,
}

fn default_s3_region() -> String {
    "us-east-1".to_string()
}

fn default_upload_storage() -> String {
    "local".to_string()
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            storage: default_upload_storage(),
            max_file_size: 52_428_800, // 50MB
            s3: S3Config::default(),
        }
    }
}

/// Internationalization / locale configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
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

        // When locales are enabled, the default locale must be in the list
        if !self.locales.is_empty() && !self.locales.contains(&self.default_locale) {
            bail!(
                "default_locale '{}' must be included in the locales list {:?}",
                self.default_locale,
                self.locales
            );
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
#[serde(default, deny_unknown_fields)]
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
#[serde(default, deny_unknown_fields)]
pub struct LiveConfig {
    /// Enable live event streaming (SSE + gRPC Subscribe). Default: true.
    pub enabled: bool,
    /// Default event delivery mode for collections/globals that don't specify one.
    /// `"metadata"` (default) = id/operation only, `"full"` = after_read hooks + data.
    pub default_mode: String,
    /// Event transport. `"memory"` (default) — in-process broadcast, events do
    /// not cross server nodes. `"redis"` — Redis pub/sub (requires the `redis`
    /// feature); events fan out to all server nodes subscribed to the same
    /// Redis instance. The Redis URL is reused from `[cache] redis_url`.
    #[serde(default = "default_live_transport")]
    pub transport: String,
    /// Broadcast channel capacity. Default: 1024.
    pub channel_capacity: usize,
    /// Maximum concurrent SSE connections (admin UI). 0 = unlimited. Default: 1000.
    pub max_sse_connections: usize,
    /// Maximum concurrent gRPC Subscribe streams. 0 = unlimited. Default: 1000.
    pub max_subscribe_connections: usize,
    /// Per-subscriber outbound send timeout (milliseconds). If forwarding an
    /// event to a specific live-update client (gRPC Subscribe or admin SSE)
    /// takes longer than this, that subscriber is dropped. Guards against a
    /// slow client holding broadcast capacity and starving other subscribers.
    /// Default: 1000 (1 second).
    pub subscriber_send_timeout_ms: u64,
}

fn default_live_transport() -> String {
    "memory".to_string()
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            default_mode: "metadata".to_string(),
            transport: default_live_transport(),
            channel_capacity: 1024,
            max_sse_connections: 1000,
            max_subscribe_connections: 1000,
            subscriber_send_timeout_ms: 1000,
        }
    }
}

/// Hook configuration — `on_init` script references and recursion limits.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
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
    /// Maximum Lua instructions per hook invocation. 0 = unlimited. Default: 10_000_000.
    pub max_instructions: u64,
    /// Maximum Lua memory in bytes per VM. 0 = unlimited. Default: 52_428_800 (50 MB).
    /// Accepts integer bytes or human-readable string ("50MB", "100MB").
    #[serde(with = "serde_filesize")]
    pub max_memory: u64,
    /// Allow Lua HTTP requests to private/internal networks. Default: false.
    pub allow_private_networks: bool,
    /// Maximum HTTP response body size in bytes for `crap.http.request`. Default: 10_485_760 (10 MB).
    /// Increase if hooks need to download large files (e.g. video processing).
    /// Accepts integer bytes or human-readable string ("10MB", "1GB").
    #[serde(with = "serde_filesize")]
    pub http_max_response_bytes: u64,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            on_init: Vec::new(),
            max_depth: 3,
            vm_pool_size: thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            max_instructions: 10_000_000,
            max_memory: 52_428_800, // 50 MB
            allow_private_networks: false,
            http_max_response_bytes: 10 * 1024 * 1024, // 10 MB
        }
    }
}

/// Access control defaults.
/// When `default_deny` is true, collections/globals without explicit access functions
/// deny all operations instead of allowing them. Default: true (secure by default).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AccessConfig {
    /// When true (default), operations on collections/globals without an explicit access
    /// function are denied. When false, missing access functions allow all.
    pub default_deny: bool,
}

impl Default for AccessConfig {
    fn default() -> Self {
        Self { default_deny: true }
    }
}

/// Log rotation strategy for file-based logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogRotation {
    /// Rotate log files every hour.
    Hourly,
    /// Rotate log files every day (default).
    #[default]
    Daily,
    /// Never rotate — single log file that grows indefinitely.
    Never,
}

/// File-based logging configuration.
///
/// When `file` is true, logs are written to rotating files in `path` (relative to
/// the config directory, or an absolute path). Disabled by default — stdout-only
/// logging is the default for backward compatibility and Docker deployments.
#[derive(Debug, Clone, Deserialize, Serialize)]
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
    fn cache_config_defaults() {
        let cache = CacheConfig::default();
        assert_eq!(cache.backend, "memory");
        assert_eq!(cache.max_entries, 10_000);
        assert_eq!(cache.max_age_secs, 0);
        assert_eq!(cache.redis_url, "redis://127.0.0.1:6379");
        assert_eq!(cache.prefix, "crap:");
    }

    #[test]
    fn cache_config_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[cache]\nbackend = \"none\"\nmax_entries = 5000\nmax_age_secs = 60\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.cache.backend, "none");
        assert_eq!(config.cache.max_entries, 5000);
        assert_eq!(config.cache.max_age_secs, 60);
    }

    #[test]
    fn cache_config_partial_toml_uses_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[cache]\nbackend = \"redis\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.cache.backend, "redis");
        assert_eq!(config.cache.max_entries, 10_000);
        assert_eq!(config.cache.redis_url, "redis://127.0.0.1:6379");
        assert_eq!(config.cache.prefix, "crap:");
    }

    #[test]
    fn cache_max_entries_zero_warns_but_passes() {
        let mut config = crate::config::CrapConfig::default();
        config.cache.max_entries = 0;
        // Should warn but not error
        assert!(config.validate().is_ok());
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
        assert_eq!(live.transport, "memory");
        assert_eq!(live.channel_capacity, 1024);
        assert_eq!(live.max_sse_connections, 1000);
        assert_eq!(live.max_subscribe_connections, 1000);
        assert_eq!(live.subscriber_send_timeout_ms, 1000);
    }

    #[test]
    fn live_config_transport_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[live]\ntransport = \"redis\"\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.live.transport, "redis");
    }

    #[test]
    fn hooks_config_defaults() {
        let hooks = HooksConfig::default();
        assert!(hooks.on_init.is_empty());
        assert_eq!(hooks.max_depth, 3);
        assert!(hooks.vm_pool_size >= 1);
        assert_eq!(hooks.max_instructions, 10_000_000);
        assert_eq!(hooks.max_memory, 52_428_800);
        assert!(!hooks.allow_private_networks);
        assert_eq!(hooks.http_max_response_bytes, 10 * 1024 * 1024);
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
    fn locale_validation_default_not_in_list_errors() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["de".to_string(), "fr".to_string()],
            fallback: true,
        };
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("default_locale"));
    }

    #[test]
    fn locale_validation_default_in_list_passes() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec!["en".to_string(), "de".to_string()],
            fallback: true,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn locale_validation_empty_locales_skips_inclusion_check() {
        let config = LocaleConfig {
            default_locale: "en".to_string(),
            locales: vec![],
            fallback: true,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn access_config_default_deny_true_by_default() {
        let config = crate::config::CrapConfig::default();
        assert!(config.access.default_deny);
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
