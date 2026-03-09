//! Configuration types loaded from `crap.toml`.

use anyhow::{Context as _, Result};
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

/// SMTP TLS mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SmtpTls {
    /// Connect plain, upgrade via STARTTLS (port 587).
    Starttls,
    /// Implicit TLS from the start (port 465).
    Tls,
    /// No encryption (local/test servers, port 25/1025).
    None,
}

impl Default for SmtpTls {
    fn default() -> Self {
        Self::Starttls
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
            anyhow::bail!("Locale code must not be empty");
        }
        if !code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            anyhow::bail!("Invalid locale code '{}': only ASCII alphanumeric, hyphens, and underscores allowed", code);
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
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct AccessConfig {
    /// When true, operations on collections/globals without an explicit access function
    /// are denied by default. When false (default), missing access functions allow all.
    pub default_deny: bool,
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
                .map(|n| n.get().clamp(4, 32))
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
    /// Max forgot-password requests per email before rate limiting. Default: 3.
    pub max_forgot_password_attempts: u32,
    /// Forgot-password rate limit window in seconds. Default: 900 (15 minutes).
    /// Accepts integer seconds or human-readable string ("15m", "900").
    #[serde(with = "serde_duration")]
    pub forgot_password_window_seconds: u64,
    /// Password strength requirements.
    #[serde(default)]
    pub password_policy: PasswordPolicy,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            secret: String::new(),
            token_expiry: 7200,
            max_login_attempts: 5,
            login_lockout_seconds: 300,
            reset_token_expiry: 3600,
            max_forgot_password_attempts: 3,
            forgot_password_window_seconds: 900,
            password_policy: PasswordPolicy::default(),
        }
    }
}

/// Password strength requirements. Applied to all password-setting paths:
/// user creation (admin, gRPC, CLI), password reset, and password update.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PasswordPolicy {
    /// Minimum password length. Default: 8.
    pub min_length: usize,
    /// Maximum password length. Default: 128. Prevents DoS via Argon2 on huge inputs.
    pub max_length: usize,
    /// Require at least one uppercase letter (A-Z). Default: false.
    pub require_uppercase: bool,
    /// Require at least one lowercase letter (a-z). Default: false.
    pub require_lowercase: bool,
    /// Require at least one digit (0-9). Default: false.
    pub require_digit: bool,
    /// Require at least one special character (non-alphanumeric). Default: false.
    pub require_special: bool,
}

impl Default for PasswordPolicy {
    fn default() -> Self {
        Self {
            min_length: 8,
            max_length: 128,
            require_uppercase: false,
            require_lowercase: false,
            require_digit: false,
            require_special: false,
        }
    }
}

impl PasswordPolicy {
    /// Validate a password against this policy. Returns `Ok(())` if the password
    /// meets all requirements, or `Err` with a human-readable message.
    pub fn validate(&self, password: &str) -> Result<()> {
        if password.len() < self.min_length {
            anyhow::bail!("Password must be at least {} characters", self.min_length);
        }
        if password.len() > self.max_length {
            anyhow::bail!("Password must be at most {} characters", self.max_length);
        }
        if self.require_uppercase && !password.chars().any(|c| c.is_ascii_uppercase()) {
            anyhow::bail!("Password must contain at least one uppercase letter");
        }
        if self.require_lowercase && !password.chars().any(|c| c.is_ascii_lowercase()) {
            anyhow::bail!("Password must contain at least one lowercase letter");
        }
        if self.require_digit && !password.chars().any(|c| c.is_ascii_digit()) {
            anyhow::bail!("Password must contain at least one digit");
        }
        if self.require_special && !password.chars().any(|c| !c.is_alphanumeric()) {
            anyhow::bail!("Password must contain at least one special character");
        }
        Ok(())
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
            config.locale.validate().context("Invalid locale configuration")?;
            Ok(config)
        } else {
            tracing::info!("No crap.toml found, using defaults");
            Ok(CrapConfig::default())
        }
    }

    /// Validate configuration for common misconfigurations.
    ///
    /// Returns errors for fatal issues (e.g., pool_max_size = 0) and logs
    /// warnings for non-fatal but suspicious settings.
    pub fn validate(&self) -> Result<()> {
        // Fatal: database pool with no connections
        if self.database.pool_max_size == 0 {
            anyhow::bail!("database.pool_max_size must be > 0");
        }

        // Fatal: instant connection timeout
        if self.database.connection_timeout == 0 {
            anyhow::bail!("database.connection_timeout must be > 0");
        }

        // Fatal: Lua VM pool with no VMs
        if self.hooks.vm_pool_size == 0 {
            anyhow::bail!("hooks.vm_pool_size must be > 0");
        }

        // Warning: no jobs will execute
        if self.jobs.max_concurrent == 0 {
            tracing::warn!("jobs.max_concurrent = 0 — no jobs will be executed");
        }

        // Warning: weak JWT signing key (when explicitly set)
        if !self.auth.secret.is_empty() && self.auth.secret.len() < 32 {
            tracing::warn!("auth.secret is shorter than 32 characters — consider using a stronger key");
        }

        // Warning: max_depth = 0 means no population will ever work
        if self.depth.max_depth == 0 {
            tracing::warn!("depth.max_depth = 0 — all depth/populate requests will be capped to 0");
        }

        Ok(())
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
        let full_match = cap.get(0).expect("regex group 0 always exists");
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
mod tests;
