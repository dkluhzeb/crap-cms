//! Configuration types loaded from `crap.toml`.

use anyhow::{Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Parse a human-readable duration string into seconds.
///
/// Supports: `"30s"` (seconds), `"30m"` (minutes), `"24h"` (hours), `"7d"` (days).
/// Returns `None` for empty or invalid input.
pub(crate) fn parse_duration_string(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num_str, suffix) = s.split_at(s.len().saturating_sub(1));
    let num: u64 = num_str.parse().ok()?;
    match suffix {
        "s" => Some(num),
        "m" => Some(num * 60),
        "h" => Some(num * 3600),
        "d" => Some(num * 86400),
        _ => None,
    }
}

/// Serde deserializer that accepts both an integer (seconds) and a human-readable
/// duration string (`"30s"`, `"5m"`, `"2h"`, `"7d"`). Used for config fields where
/// backward compatibility with plain integer seconds is desired.
mod serde_duration {
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
mod serde_duration_option {
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
mod serde_duration_ms {
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

/// Serde deserializer that accepts both an integer (bytes) and a human-readable
/// file size string (`"500B"`, `"100KB"`, `"50MB"`, `"1GB"`). Used for config fields.
mod serde_filesize {
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

/// Top-level configuration loaded from `crap.toml` in the config directory.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct CrapConfig {
    /// Required CMS version. If set, warns on mismatch at startup.
    pub crap_version: Option<String>,
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub admin: AdminConfig,
    pub hooks: HooksConfig,
    pub auth: AuthConfig,
    pub depth: DepthConfig,
    pub upload: UploadConfig,
    pub email: EmailConfig,
    pub live: LiveConfig,
    pub locale: LocaleConfig,
    pub jobs: JobsConfig,
    pub cors: CorsConfig,
    pub access: AccessConfig,
    pub pagination: PaginationConfig,
    pub mcp: McpConfig,
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
    Page,
    Cursor,
}

/// MCP (Model Context Protocol) server configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
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

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            http: false,
            config_tools: false,
            api_key: String::new(),
            include_collections: Vec::new(),
            exclude_collections: Vec::new(),
        }
    }
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

/// CORS (Cross-Origin Resource Sharing) configuration.
/// Empty `allowed_origins` = CORS layer not added (default, backward compatible).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct CorsConfig {
    /// Origins allowed to make cross-origin requests. Empty = CORS disabled.
    /// Use `["*"]` to allow any origin (not compatible with `allow_credentials`).
    pub allowed_origins: Vec<String>,
    /// HTTP methods allowed in CORS requests.
    pub allowed_methods: Vec<String>,
    /// Request headers allowed in CORS requests.
    pub allowed_headers: Vec<String>,
    /// Response headers exposed to the browser.
    pub exposed_headers: Vec<String>,
    /// How long browsers can cache preflight results, in seconds.
    /// Accepts integer seconds or human-readable string ("1h", "3600").
    #[serde(with = "serde_duration")]
    pub max_age_seconds: u64,
    /// Whether to allow credentials (cookies, Authorization header).
    /// Cannot be used with `allowed_origins = ["*"]`.
    pub allow_credentials: bool,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowed_origins: Vec::new(),
            allowed_methods: vec![
                "GET".into(), "POST".into(), "PUT".into(),
                "DELETE".into(), "PATCH".into(), "OPTIONS".into(),
            ],
            allowed_headers: vec![
                "Content-Type".into(), "Authorization".into(),
            ],
            exposed_headers: Vec::new(),
            max_age_seconds: 3600,
            allow_credentials: false,
        }
    }
}

impl CorsConfig {
    /// Build a tower-http CorsLayer from this config. Returns None if no origins configured.
    pub fn build_layer(&self) -> Option<tower_http::cors::CorsLayer> {
        if self.allowed_origins.is_empty() {
            return None;
        }
        use tower_http::cors::CorsLayer;
        use axum::http::{HeaderName, Method};
        use std::str::FromStr;

        let is_wildcard = self.allowed_origins.len() == 1 && self.allowed_origins[0] == "*";

        // Validate: wildcard + credentials is invalid per CORS spec
        if is_wildcard && self.allow_credentials {
            tracing::warn!(
                "CORS: allow_credentials is incompatible with wildcard origin '*'. \
                 Ignoring allow_credentials."
            );
        }

        let origin = if is_wildcard {
            tower_http::cors::AllowOrigin::any()
        } else {
            tower_http::cors::AllowOrigin::list(
                self.allowed_origins.iter()
                    .filter_map(|o| o.parse().ok())
            )
        };

        let methods = tower_http::cors::AllowMethods::list(
            self.allowed_methods.iter()
                .filter_map(|m| Method::from_str(m).ok())
        );

        let headers = tower_http::cors::AllowHeaders::list(
            self.allowed_headers.iter()
                .filter_map(|h| HeaderName::from_str(h).ok())
        );

        let mut layer = CorsLayer::new()
            .allow_origin(origin)
            .allow_methods(methods)
            .allow_headers(headers)
            .max_age(std::time::Duration::from_secs(self.max_age_seconds));

        if !self.exposed_headers.is_empty() {
            layer = layer.expose_headers(
                self.exposed_headers.iter()
                    .filter_map(|h| HeaderName::from_str(h).ok())
                    .collect::<Vec<_>>()
            );
        }

        // Only set credentials when not using wildcard origin
        if self.allow_credentials && !is_wildcard {
            layer = layer.allow_credentials(true);
        }

        Some(layer)
    }
}

/// Access control defaults.
/// When `default_deny` is true, collections/globals without explicit access functions
/// deny all operations instead of allowing them. Default: false (backward compatible).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AccessConfig {
    /// When true, operations on collections/globals without an explicit access function
    /// are denied by default. When false (default), missing access functions allow all.
    pub default_deny: bool,
}

impl Default for AccessConfig {
    fn default() -> Self {
        Self {
            default_deny: false,
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
                .map(|n| n.get().max(4).min(32))
                .unwrap_or(8),
        }
    }
}

/// JWT authentication settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AuthConfig {
    /// JWT secret. If empty, a random secret is generated at startup (tokens
    /// won't survive restarts).
    pub secret: String,
    /// Default token expiry in seconds (can be overridden per-collection).
    /// Accepts integer seconds or human-readable string ("2h", "7200").
    #[serde(with = "serde_duration")]
    pub token_expiry: u64,
    /// Max failed login attempts before lockout. Default: 5.
    pub max_login_attempts: u32,
    /// Lockout window in seconds. Default: 300 (5 minutes).
    /// Accepts integer seconds or human-readable string ("5m", "300").
    #[serde(with = "serde_duration")]
    pub login_lockout_seconds: u64,
    /// Password reset token expiry in seconds. Default: 3600 (1 hour).
    /// Accepts integer seconds or human-readable string ("1h", "3600").
    #[serde(with = "serde_duration")]
    pub reset_token_expiry: u64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            secret: String::new(),
            token_expiry: 7200,
            max_login_attempts: 5,
            login_lockout_seconds: 300,
            reset_token_expiry: 3600,
        }
    }
}

/// Response compression mode for the admin HTTP server.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CompressionMode {
    #[default]
    Off,
    Gzip,
    Br,
    All,
}

/// Admin UI and gRPC server bind settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    pub admin_port: u16,
    pub grpc_port: u16,
    pub host: String,
    /// Enable response compression. Default: off (most deployments use a reverse proxy).
    /// Options: "off", "gzip", "br", "all".
    pub compression: CompressionMode,
    /// Enable gRPC server reflection (allows clients to discover services).
    /// Default: true. Disable in production to hide API surface.
    pub grpc_reflection: bool,
    /// Per-IP gRPC rate limit: max requests per window. 0 = disabled (default).
    pub grpc_rate_limit_requests: u32,
    /// Sliding window duration in seconds for gRPC rate limiting.
    #[serde(default = "default_grpc_rate_limit_window", with = "serde_duration")]
    pub grpc_rate_limit_window: u64,
}

fn default_grpc_rate_limit_window() -> u64 {
    60
}

/// SQLite database path and pool configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub path: String,
    /// Maximum number of connections in the pool. Default: 16.
    pub pool_max_size: u32,
    /// SQLite busy timeout in milliseconds. Default: 30000 (30s).
    /// Accepts integer milliseconds or human-readable string ("30s", "1m").
    #[serde(with = "serde_duration_ms")]
    pub busy_timeout: u64,
    /// Pool connection timeout in seconds. Default: 5.
    /// Accepts integer seconds or human-readable string ("5s", "10s").
    #[serde(with = "serde_duration")]
    pub connection_timeout: u64,
}

/// Admin UI behavior settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AdminConfig {
    pub dev_mode: bool,
    /// When true (default), block admin panel if no auth collection exists.
    /// Set to false for open dev mode with no authentication.
    pub require_auth: bool,
    /// Optional Lua function ref that gates admin panel access.
    /// Checked after successful authentication. None = any authenticated user.
    pub access: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            admin_port: 3000,
            grpc_port: 50051,
            host: "0.0.0.0".to_string(),
            compression: CompressionMode::Off,
            grpc_reflection: true,
            grpc_rate_limit_requests: 0,
            grpc_rate_limit_window: 60,
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: "data/crap.db".to_string(),
            pool_max_size: 32,
            busy_timeout: 30000,
            connection_timeout: 5,
        }
    }
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            dev_mode: false,
            require_auth: true,
            access: None,
        }
    }
}

impl CrapConfig {
    /// Load configuration from `crap.toml` in the config directory, falling back to defaults.
    ///
    /// Supports environment variable substitution: `${VAR}` is replaced with the
    /// value of `VAR` from the environment. `${VAR:-default}` uses `default` if
    /// `VAR` is unset or empty. A reference to an unset variable without a default
    /// causes an error.
    pub fn load(config_dir: &Path) -> Result<Self> {
        let config_path = config_dir.join("crap.toml");
        if config_path.exists() {
            let contents = std::fs::read_to_string(&config_path)
                .with_context(|| format!("Failed to read {}", config_path.display()))?;
            // Parse TOML first (strips comments), then substitute env vars only in string values.
            // This avoids errors from `${VAR}` patterns in comments.
            let mut value: toml::Value = toml::from_str(&contents)
                .with_context(|| format!("Failed to parse {}", config_path.display()))?;
            substitute_in_value(&mut value)?;
            let config: CrapConfig = value.try_into()
                .with_context(|| format!("Failed to deserialize {}", config_path.display()))?;
            Ok(config)
        } else {
            tracing::info!("No crap.toml found, using defaults");
            Ok(CrapConfig::default())
        }
    }

    /// Check `crap_version` against the running binary version.
    ///
    /// Returns `None` if the version is unset or matches. Returns `Some(message)`
    /// with a human-readable warning if there is a mismatch. Callers should log
    /// the message via `tracing::warn!`.
    ///
    /// Supports exact match (`"0.1.0"`) and prefix match (`"0.1"` matches any `0.1.x`).
    pub fn check_version(&self) -> Option<String> {
        Self::check_version_against(self.crap_version.as_deref(), env!("CARGO_PKG_VERSION"))
    }

    /// Version check against an explicit version string, testable without
    /// depending on the compile-time package version.
    pub fn check_version_against(crap_version: Option<&str>, pkg_version: &str) -> Option<String> {
        let required = match crap_version {
            Some(v) if !v.is_empty() => v,
            _ => return None,
        };

        // Exact match
        if required == pkg_version {
            return None;
        }

        // Prefix match: "0.1" should match "0.1.0", "0.1.3", etc.
        // The required string must be a proper prefix followed by a dot in the pkg version.
        if pkg_version.starts_with(required)
            && pkg_version.as_bytes().get(required.len()) == Some(&b'.')
        {
            return None;
        }

        Some(format!(
            "crap_version mismatch: config requires \"{}\", but running version is \"{}\"",
            required, pkg_version
        ))
    }

    /// Resolve the database path relative to the config directory.
    #[must_use]
    pub fn db_path(&self, config_dir: &Path) -> PathBuf {
        let p = Path::new(&self.database.path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            config_dir.join(p)
        }
    }
}

/// Recursively walk a TOML `Value` tree and substitute `${VAR}` / `${VAR:-default}`
/// in all `String` nodes. Tables and arrays are descended into; other types are untouched.
fn substitute_in_value(value: &mut toml::Value) -> Result<()> {
    match value {
        toml::Value::String(s) => {
            *s = substitute_env_vars(s)?;
        }
        toml::Value::Array(arr) => {
            for item in arr.iter_mut() {
                substitute_in_value(item)?;
            }
        }
        toml::Value::Table(tbl) => {
            for (_key, val) in tbl.iter_mut() {
                substitute_in_value(val)?;
            }
        }
        _ => {} // Integer, Float, Boolean, Datetime — no substitution
    }
    Ok(())
}

/// Replace `${VAR}` and `${VAR:-default}` placeholders with environment variable values.
///
/// - `${VAR}` — replaced with the value of `VAR`. Returns an error if `VAR` is unset.
/// - `${VAR:-fallback}` — replaced with `VAR` if set and non-empty, otherwise `fallback`.
fn substitute_env_vars(input: &str) -> Result<String> {
    let re = Regex::new(r"\$\{([^}]+)\}").expect("env var regex");
    let mut result = String::with_capacity(input.len());
    let mut last_end = 0;

    for cap in re.captures_iter(input) {
        let full_match = cap.get(0).unwrap();
        result.push_str(&input[last_end..full_match.start()]);

        let inner = &cap[1];
        if let Some((var_name, default_val)) = inner.split_once(":-") {
            match std::env::var(var_name) {
                Ok(val) if !val.is_empty() => result.push_str(&val),
                _ => result.push_str(default_val),
            }
        } else {
            let val = std::env::var(inner).with_context(|| {
                format!(
                    "Environment variable '{}' referenced in crap.toml is not set \
                     (use ${{{}:-default}} for a fallback)",
                    inner, inner
                )
            })?;
            result.push_str(&val);
        }

        last_end = full_match.end();
    }

    result.push_str(&input[last_end..]);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_values() {
        let config = CrapConfig::default();
        assert_eq!(config.server.admin_port, 3000);
        assert_eq!(config.server.grpc_port, 50051);
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.database.path, "data/crap.db");
        assert_eq!(config.database.pool_max_size, 32);
        assert_eq!(config.database.busy_timeout, 30000);
        assert!(!config.admin.dev_mode);
        assert!(config.admin.require_auth);
        assert!(config.admin.access.is_none());
        assert!(!config.access.default_deny);
        assert_eq!(config.pagination.default_limit, 20);
        assert_eq!(config.pagination.max_limit, 1000);
        assert_eq!(config.pagination.mode, PaginationMode::Page);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let dir = std::path::PathBuf::from("/tmp/nonexistent-crap-test");
        let config = CrapConfig::load(&dir).unwrap();
        assert_eq!(config.server.admin_port, 3000);
    }

    #[test]
    fn db_path_relative() {
        let config = CrapConfig::default();
        let dir = Path::new("/my/config");
        assert_eq!(config.db_path(dir), Path::new("/my/config/data/crap.db"));
    }

    #[test]
    fn db_path_absolute() {
        let mut config = CrapConfig::default();
        config.database.path = "/absolute/path.db".to_string();
        let dir = Path::new("/my/config");
        assert_eq!(config.db_path(dir), Path::new("/absolute/path.db"));
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
        assert!(with_locales.is_enabled(), "non-empty locales should be enabled");
    }

    #[test]
    fn load_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            r#"
[server]
admin_port = 4000
grpc_port = 60000
host = "127.0.0.1"

[database]
path = "mydata/custom.db"

[admin]
dev_mode = false
"#,
        ).unwrap();

        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.server.admin_port, 4000);
        assert_eq!(config.server.grpc_port, 60000);
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.database.path, "mydata/custom.db");
        assert!(!config.admin.dev_mode);
    }

    #[test]
    fn email_config_defaults() {
        let email = EmailConfig::default();
        assert!(email.smtp_host.is_empty(), "smtp_host should be empty by default");
        assert_eq!(email.smtp_port, 587);
        assert!(email.smtp_user.is_empty());
        assert!(email.smtp_pass.is_empty());
        assert_eq!(email.from_address, "noreply@example.com");
        assert_eq!(email.from_name, "Crap CMS");
    }

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

    // -- serde_filesize deserialization tests --

    #[test]
    fn serde_filesize_integer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[upload]\nmax_file_size = 52428800\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.upload.max_file_size, 52_428_800);
    }

    #[test]
    fn serde_filesize_string_megabytes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[upload]\nmax_file_size = \"50MB\"\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.upload.max_file_size, 50 * 1024 * 1024);
    }

    #[test]
    fn serde_filesize_string_gigabytes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[upload]\nmax_file_size = \"1GB\"\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.upload.max_file_size, 1024 * 1024 * 1024);
    }

    // -- serde_duration deserialization tests --

    #[test]
    fn serde_duration_integer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[auth]\ntoken_expiry = 7200\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.auth.token_expiry, 7200);
    }

    #[test]
    fn serde_duration_string_hours() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[auth]\ntoken_expiry = \"2h\"\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.auth.token_expiry, 7200);
    }

    #[test]
    fn serde_duration_string_minutes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[auth]\nlogin_lockout_seconds = \"5m\"\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.auth.login_lockout_seconds, 300);
    }

    #[test]
    fn serde_duration_ms_human_string() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[database]\nbusy_timeout = \"30s\"\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.database.busy_timeout, 30000);
    }

    #[test]
    fn serde_duration_ms_integer_backward_compat() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[database]\nbusy_timeout = 15000\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.database.busy_timeout, 15000);
    }

    #[test]
    fn connection_timeout_human_string() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[database]\nconnection_timeout = \"10s\"\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.database.connection_timeout, 10);
    }

    #[test]
    fn serde_duration_option_string() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs]\nauto_purge = \"7d\"\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.auto_purge, Some(7 * 86400));
    }

    #[test]
    fn serde_duration_option_integer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs]\nauto_purge = 86400\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.auto_purge, Some(86400));
    }

    #[test]
    fn serde_duration_option_absent_uses_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs]\nmax_concurrent = 5\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.auto_purge, Some(7 * 86400)); // default
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
    fn depth_config_defaults() {
        let depth = DepthConfig::default();
        assert_eq!(depth.default_depth, 1);
        assert_eq!(depth.max_depth, 10);
    }

    #[test]
    fn auth_config_defaults() {
        let auth = AuthConfig::default();
        assert!(auth.secret.is_empty());
        assert_eq!(auth.token_expiry, 7200);
        assert_eq!(auth.reset_token_expiry, 3600);
    }

    #[test]
    fn hooks_config_defaults() {
        let hooks = HooksConfig::default();
        assert!(hooks.on_init.is_empty());
        assert_eq!(hooks.max_depth, 3);
        assert!(hooks.vm_pool_size >= 4 && hooks.vm_pool_size <= 32);
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
    fn cors_config_defaults() {
        let cors = CorsConfig::default();
        assert!(cors.allowed_origins.is_empty());
        assert_eq!(cors.allowed_methods, vec!["GET", "POST", "PUT", "DELETE", "PATCH", "OPTIONS"]);
        assert_eq!(cors.allowed_headers, vec!["Content-Type", "Authorization"]);
        assert!(cors.exposed_headers.is_empty());
        assert_eq!(cors.max_age_seconds, 3600);
        assert!(!cors.allow_credentials);
    }

    #[test]
    fn cors_build_layer_disabled_when_no_origins() {
        let cors = CorsConfig::default();
        assert!(cors.build_layer().is_none());
    }

    #[test]
    fn cors_build_layer_wildcard() {
        let cors = CorsConfig {
            allowed_origins: vec!["*".to_string()],
            ..Default::default()
        };
        assert!(cors.build_layer().is_some());
    }

    #[test]
    fn cors_build_layer_specific_origins() {
        let cors = CorsConfig {
            allowed_origins: vec![
                "http://localhost:3000".to_string(),
                "https://example.com".to_string(),
            ],
            ..Default::default()
        };
        assert!(cors.build_layer().is_some());
    }

    #[test]
    fn cors_build_layer_with_credentials() {
        let cors = CorsConfig {
            allowed_origins: vec!["https://example.com".to_string()],
            allow_credentials: true,
            ..Default::default()
        };
        assert!(cors.build_layer().is_some());
    }

    #[test]
    fn cors_build_layer_wildcard_with_credentials_ignores_credentials() {
        // Wildcard + credentials is invalid per CORS spec — credentials should be ignored
        let cors = CorsConfig {
            allowed_origins: vec!["*".to_string()],
            allow_credentials: true,
            ..Default::default()
        };
        // Should still build (doesn't panic), just logs a warning
        assert!(cors.build_layer().is_some());
    }

    #[test]
    fn cors_build_layer_with_exposed_headers() {
        let cors = CorsConfig {
            allowed_origins: vec!["https://example.com".to_string()],
            exposed_headers: vec!["X-Custom-Header".to_string()],
            ..Default::default()
        };
        assert!(cors.build_layer().is_some());
    }

    #[test]
    fn cors_config_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            r#"
[cors]
allowed_origins = ["https://example.com", "https://app.example.com"]
allowed_methods = ["GET", "POST"]
allowed_headers = ["Content-Type", "Authorization", "X-Custom"]
exposed_headers = ["X-Request-Id"]
max_age_seconds = 7200
allow_credentials = true
"#,
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.cors.allowed_origins, vec!["https://example.com", "https://app.example.com"]);
        assert_eq!(config.cors.allowed_methods, vec!["GET", "POST"]);
        assert_eq!(config.cors.allowed_headers, vec!["Content-Type", "Authorization", "X-Custom"]);
        assert_eq!(config.cors.exposed_headers, vec!["X-Request-Id"]);
        assert_eq!(config.cors.max_age_seconds, 7200);
        assert!(config.cors.allow_credentials);
    }

    #[test]
    fn load_invalid_toml_returns_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("crap.toml"), "invalid { toml").unwrap();
        let result = CrapConfig::load(tmp.path());
        assert!(result.is_err());
    }

    #[test]
    fn load_partial_toml_uses_defaults_for_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[server]\nadmin_port = 5000\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.server.admin_port, 5000);
        // Other fields should be defaults
        assert_eq!(config.server.grpc_port, 50051);
        assert_eq!(config.database.path, "data/crap.db");
    }

    #[test]
    fn admin_config_defaults() {
        let admin = AdminConfig::default();
        assert!(!admin.dev_mode);
        assert!(admin.require_auth);
        assert!(admin.access.is_none());
    }

    #[test]
    fn admin_config_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[admin]\ndev_mode = true\nrequire_auth = false\naccess = \"access.admin_panel\"\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(config.admin.dev_mode);
        assert!(!config.admin.require_auth);
        assert_eq!(config.admin.access, Some("access.admin_panel".to_string()));
    }

    #[test]
    fn admin_config_partial_toml_uses_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[admin]\ndev_mode = true\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(config.admin.dev_mode);
        assert!(config.admin.require_auth); // default
        assert!(config.admin.access.is_none()); // default
    }

    #[test]
    fn access_config_default_deny_false_by_default() {
        let config = CrapConfig::default();
        assert!(!config.access.default_deny);
    }

    #[test]
    fn access_config_default_deny_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[access]\ndefault_deny = true\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(config.access.default_deny);
    }

    #[test]
    fn database_config_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[database]\npool_max_size = 32\nbusy_timeout = 60000\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.database.pool_max_size, 32);
        assert_eq!(config.database.busy_timeout, 60000);
    }

    #[test]
    fn auth_reset_token_expiry_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[auth]\nreset_token_expiry = 1800\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.auth.reset_token_expiry, 1800);
    }

    #[test]
    fn jobs_image_queue_batch_size_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[jobs]\nimage_queue_batch_size = 50\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.jobs.image_queue_batch_size, 50);
    }

    // -- crap_version / check_version tests --

    #[test]
    fn check_version_none_returns_ok() {
        assert!(CrapConfig::check_version_against(None, "0.1.0").is_none());
    }

    #[test]
    fn check_version_empty_returns_ok() {
        assert!(CrapConfig::check_version_against(Some(""), "0.1.0").is_none());
    }

    #[test]
    fn check_version_exact_match() {
        assert!(CrapConfig::check_version_against(Some("0.1.0"), "0.1.0").is_none());
    }

    #[test]
    fn check_version_prefix_match() {
        assert!(CrapConfig::check_version_against(Some("0.1"), "0.1.0").is_none());
        assert!(CrapConfig::check_version_against(Some("0.1"), "0.1.3").is_none());
        assert!(CrapConfig::check_version_against(Some("0"), "0.1.0").is_none());
    }

    #[test]
    fn check_version_prefix_no_false_positive() {
        // "0.1" should NOT match "0.10.0" — the next char must be '.'
        let result = CrapConfig::check_version_against(Some("0.1"), "0.10.0");
        assert!(result.is_some());
    }

    #[test]
    fn check_version_mismatch() {
        let result = CrapConfig::check_version_against(Some("0.2.0"), "0.1.0");
        assert!(result.is_some());
        let msg = result.unwrap();
        assert!(msg.contains("0.2.0"), "should mention required version");
        assert!(msg.contains("0.1.0"), "should mention running version");
    }

    #[test]
    fn check_version_prefix_mismatch() {
        let result = CrapConfig::check_version_against(Some("1.0"), "0.1.0");
        assert!(result.is_some());
    }

    #[test]
    fn check_version_from_struct() {
        // Test via the public check_version() method on a default config (crap_version = None)
        let config = CrapConfig::default();
        assert!(config.crap_version.is_none());
        assert!(config.check_version().is_none());
    }

    #[test]
    fn check_version_from_struct_with_value() {
        let mut config = CrapConfig::default();
        config.crap_version = Some(env!("CARGO_PKG_VERSION").to_string());
        assert!(config.check_version().is_none());
    }

    #[test]
    fn crap_version_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "crap_version = \"0.1.0\"\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.crap_version, Some("0.1.0".to_string()));
    }

    #[test]
    fn crap_version_absent_in_toml_is_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[server]\nadmin_port = 3000\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(config.crap_version.is_none());
    }

    // -- substitute_env_vars tests --

    #[test]
    fn env_subst_simple() {
        std::env::set_var("CRAP_TEST_HOST", "127.0.0.1");
        let result = substitute_env_vars("host = \"${CRAP_TEST_HOST}\"").unwrap();
        assert_eq!(result, "host = \"127.0.0.1\"");
        std::env::remove_var("CRAP_TEST_HOST");
    }

    #[test]
    fn env_subst_with_default() {
        std::env::remove_var("CRAP_TEST_MISSING");
        let result = substitute_env_vars("port = ${CRAP_TEST_MISSING:-3000}").unwrap();
        assert_eq!(result, "port = 3000");
    }

    #[test]
    fn env_subst_default_not_used_when_set() {
        std::env::set_var("CRAP_TEST_PORT", "8080");
        let result = substitute_env_vars("port = ${CRAP_TEST_PORT:-3000}").unwrap();
        assert_eq!(result, "port = 8080");
        std::env::remove_var("CRAP_TEST_PORT");
    }

    #[test]
    fn env_subst_empty_uses_default() {
        std::env::set_var("CRAP_TEST_EMPTY", "");
        let result = substitute_env_vars("val = \"${CRAP_TEST_EMPTY:-fallback}\"").unwrap();
        assert_eq!(result, "val = \"fallback\"");
        std::env::remove_var("CRAP_TEST_EMPTY");
    }

    #[test]
    fn env_subst_missing_no_default_errors() {
        std::env::remove_var("CRAP_TEST_NOEXIST_XYZ");
        let result = substitute_env_vars("secret = \"${CRAP_TEST_NOEXIST_XYZ}\"");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("CRAP_TEST_NOEXIST_XYZ"));
    }

    #[test]
    fn env_subst_multiple() {
        std::env::set_var("CRAP_TEST_A", "hello");
        std::env::set_var("CRAP_TEST_B", "world");
        let result = substitute_env_vars("${CRAP_TEST_A} ${CRAP_TEST_B}").unwrap();
        assert_eq!(result, "hello world");
        std::env::remove_var("CRAP_TEST_A");
        std::env::remove_var("CRAP_TEST_B");
    }

    #[test]
    fn env_subst_no_vars_passthrough() {
        let input = "admin_port = 3000\nhost = \"0.0.0.0\"";
        let result = substitute_env_vars(input).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn env_subst_in_toml_load() {
        std::env::set_var("CRAP_TEST_ADMIN_PORT", "9999");
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[server]\nadmin_port = 9999\nhost = \"${CRAP_TEST_HOST2:-0.0.0.0}\"\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.server.admin_port, 9999);
        assert_eq!(config.server.host, "0.0.0.0");
        std::env::remove_var("CRAP_TEST_ADMIN_PORT");
    }

    #[test]
    fn env_subst_ignores_comments() {
        // ${UNSET_VAR_IN_COMMENT} in a TOML comment should not cause an error
        std::env::remove_var("CRAP_TEST_UNSET_COMMENT_VAR");
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "# Set ${CRAP_TEST_UNSET_COMMENT_VAR} for production\n\
             [server]\nadmin_port = 3000\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.server.admin_port, 3000);
    }

    #[test]
    fn env_subst_in_string_values_via_load() {
        std::env::set_var("CRAP_TEST_SMTP_HOST", "mail.example.com");
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[email]\nsmtp_host = \"${CRAP_TEST_SMTP_HOST}\"\n",
        ).unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.email.smtp_host, "mail.example.com");
        std::env::remove_var("CRAP_TEST_SMTP_HOST");
    }

    // -- substitute_in_value tests --

    #[test]
    fn substitute_in_value_string() {
        std::env::set_var("CRAP_TEST_SIV", "replaced");
        let mut val = toml::Value::String("${CRAP_TEST_SIV}".to_string());
        substitute_in_value(&mut val).unwrap();
        assert_eq!(val.as_str().unwrap(), "replaced");
        std::env::remove_var("CRAP_TEST_SIV");
    }

    #[test]
    fn substitute_in_value_table() {
        std::env::set_var("CRAP_TEST_SIV2", "value2");
        let mut tbl = toml::map::Map::new();
        tbl.insert("key".to_string(), toml::Value::String("${CRAP_TEST_SIV2}".to_string()));
        tbl.insert("num".to_string(), toml::Value::Integer(42));
        let mut val = toml::Value::Table(tbl);
        substitute_in_value(&mut val).unwrap();
        assert_eq!(val.get("key").unwrap().as_str().unwrap(), "value2");
        assert_eq!(val.get("num").unwrap().as_integer().unwrap(), 42);
        std::env::remove_var("CRAP_TEST_SIV2");
    }

    #[test]
    fn substitute_in_value_array() {
        std::env::set_var("CRAP_TEST_SIV3", "item");
        let mut val = toml::Value::Array(vec![
            toml::Value::String("${CRAP_TEST_SIV3}".to_string()),
            toml::Value::Boolean(true),
        ]);
        substitute_in_value(&mut val).unwrap();
        assert_eq!(val.as_array().unwrap()[0].as_str().unwrap(), "item");
        assert!(val.as_array().unwrap()[1].as_bool().unwrap());
        std::env::remove_var("CRAP_TEST_SIV3");
    }

    #[test]
    fn substitute_in_value_non_string_untouched() {
        let mut val = toml::Value::Integer(99);
        substitute_in_value(&mut val).unwrap();
        assert_eq!(val.as_integer().unwrap(), 99);

        let mut val = toml::Value::Float(3.14);
        substitute_in_value(&mut val).unwrap();
        assert!((val.as_float().unwrap() - 3.14).abs() < f64::EPSILON);

        let mut val = toml::Value::Boolean(true);
        substitute_in_value(&mut val).unwrap();
        assert!(val.as_bool().unwrap());
    }
}
