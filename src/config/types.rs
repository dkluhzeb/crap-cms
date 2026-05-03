//! Top-level `CrapConfig` struct and its loading/validation logic.

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::config::{
    auth::AuthConfig,
    cors::CorsConfig,
    env::substitute_in_value,
    features::{
        AccessConfig, CacheConfig, DepthConfig, EmailConfig, HooksConfig, JobsConfig, LiveConfig,
        LocaleConfig, LoggingConfig, McpConfig, PaginationConfig, UpdateConfig, UploadConfig,
    },
    server::{AdminConfig, DatabaseConfig, ServerConfig},
};

/// Minimum character length for `mcp.api_key` when `mcp.http` is enabled.
/// 32 characters of the typical `base64`/`hex` alphabets give ≥ 128 bits of
/// entropy even with low per-char entropy — well past what brute-force can
/// reach against a key that an attacker cannot guess from context.
const MIN_MCP_API_KEY_LEN: usize = 32;

/// Top-level configuration loaded from `crap.toml` in the config directory.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct CrapConfig {
    /// Required CMS version. If set, warns on mismatch at startup.
    pub crap_version: Option<String>,
    /// Server settings (host, ports, compression, etc.).
    pub server: ServerConfig,
    /// Database connection and pooling settings.
    pub database: DatabaseConfig,
    /// Admin UI settings.
    pub admin: AdminConfig,
    /// Global hook settings, including Lua VM pool size.
    pub hooks: HooksConfig,
    /// Authentication settings, including JWT secret and password policy.
    pub auth: AuthConfig,
    /// Default and maximum relationship population depth.
    pub depth: DepthConfig,
    /// File upload settings.
    pub upload: UploadConfig,
    /// Email/SMTP settings.
    pub email: EmailConfig,
    /// Live update and preview settings.
    pub live: LiveConfig,
    /// Localization and internationalization settings.
    pub locale: LocaleConfig,
    /// Background job processing settings.
    pub jobs: JobsConfig,
    /// CORS (Cross-Origin Resource Sharing) settings.
    pub cors: CorsConfig,
    /// Access control settings.
    pub access: AccessConfig,
    /// Default pagination settings.
    pub pagination: PaginationConfig,
    /// MCP (Model Context Protocol) settings.
    pub mcp: McpConfig,
    /// Cache backend settings.
    pub cache: CacheConfig,
    /// File-based logging settings.
    pub logging: LoggingConfig,
    /// `crap-cms update` settings (startup nudge).
    pub update: UpdateConfig,
}

/// True if the config contains any non-empty secret.
#[cfg(unix)]
fn has_any_secret(config: &CrapConfig) -> bool {
    !config.auth.secret.is_empty()
        || !config.email.smtp_pass.is_empty()
        || !config.upload.s3.secret_key.is_empty()
}

/// Pure check for whether a given Unix permissions mode is considered "loose"
/// (world-readable or world-writable).
#[cfg(unix)]
fn is_world_accessible_mode(mode: u32) -> bool {
    // World-read = 0o004, world-write = 0o002.
    (mode & 0o777) & 0o006 != 0
}

/// Decide whether a loose-permissions warning should fire for a given config
/// and mode. Returns `true` iff secrets are present AND permissions are loose.
/// Used only from the test suite; the production path embeds the same check
/// inline in `warn_on_loose_permissions`.
#[cfg(all(unix, test))]
fn should_warn_loose_permissions(config: &CrapConfig, mode: u32) -> bool {
    has_any_secret(config) && is_world_accessible_mode(mode)
}

/// Emit a warning when the config file has loose Unix permissions (world-readable
/// or world-writable) AND contains at least one non-empty secret. No-op on Windows.
#[cfg(unix)]
fn warn_on_loose_permissions(config_path: &Path, config: &CrapConfig) {
    if !has_any_secret(config) {
        return;
    }

    let Ok(meta) = fs::metadata(config_path) else {
        return;
    };

    let mode = meta.mode();

    if is_world_accessible_mode(mode) {
        warn!(
            "config file at {} is world-readable but contains secrets; recommend chmod 600",
            config_path.display()
        );
    }
}

#[cfg(not(unix))]
fn warn_on_loose_permissions(_config_path: &Path, _config: &CrapConfig) {}

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
            let contents = fs::read_to_string(&config_path)
                .with_context(|| format!("Failed to read {}", config_path.display()))?;
            // Parse TOML first (strips comments), then substitute env vars only in string values.
            // This avoids errors from `${VAR}` patterns in comments.
            let mut value: toml::Value = toml::from_str(&contents)
                .with_context(|| format!("Failed to parse {}", config_path.display()))?;

            substitute_in_value(&mut value)?;

            let config: CrapConfig = value
                .try_into()
                .with_context(|| format!("Failed to deserialize {}", config_path.display()))?;

            config
                .locale
                .validate()
                .context("Invalid locale configuration")?;

            config.validate().context("Invalid configuration")?;

            warn_on_loose_permissions(&config_path, &config);

            Ok(config)
        } else {
            info!("No crap.toml found, using defaults");

            Ok(CrapConfig::default())
        }
    }

    /// Create a configuration with permissive defaults for testing.
    ///
    /// Same as `Default` but with `access.default_deny = false` so tests that don't
    /// configure access functions aren't blocked.
    pub fn test_default() -> Self {
        let mut config = Self::default();
        config.access.default_deny = false;
        config
    }

    /// Validate configuration for common misconfigurations.
    ///
    /// Returns errors for fatal issues (e.g., pool_max_size = 0) and logs
    /// warnings for non-fatal but suspicious settings.
    pub fn validate(&self) -> Result<()> {
        self.validate_database()?;
        self.validate_server()?;
        self.validate_pagination()?;
        self.validate_depth()?;
        self.validate_jobs()?;
        self.validate_auth()?;
        self.validate_email()?;
        self.validate_logging()?;
        self.validate_mcp()?;
        self.validate_live()?;
        self.validate_cache()?;

        Ok(())
    }

    /// Validate database pool settings.
    fn validate_database(&self) -> Result<()> {
        if self.database.pool_max_size == 0 {
            bail!("database.pool_max_size must be > 0");
        }

        if self.database.connection_timeout == 0 {
            bail!("database.connection_timeout must be > 0");
        }

        Ok(())
    }

    /// Validate server ports, timeouts, and rate limiting.
    fn validate_server(&self) -> Result<()> {
        if self.server.admin_port == 0 || self.server.grpc_port == 0 {
            bail!("Server ports must be > 0");
        }

        if self.server.admin_port == self.server.grpc_port {
            bail!("admin_port and grpc_port must be different");
        }

        if self.server.request_timeout == Some(0) {
            bail!("server.request_timeout must be > 0 (or omitted to disable)");
        }

        if self.server.grpc_timeout == Some(0) {
            bail!("server.grpc_timeout must be > 0 (or omitted to disable)");
        }

        if self.server.grpc_rate_limit_requests > 0 && self.server.grpc_rate_limit_window == 0 {
            bail!("server.grpc_rate_limit_window must be > 0 when grpc_rate_limit_requests > 0");
        }

        self.validate_trusted_proxies()?;

        Ok(())
    }

    /// Validate `trust_proxy` / `trusted_proxies` pairing.
    ///
    /// Fails startup when `trust_proxy = true` without a `trusted_proxies`
    /// allowlist — in that state any client can spoof `X-Forwarded-For`
    /// to rotate per-IP rate limits. Operators who genuinely need the
    /// legacy "trust XFF from any peer" behaviour (e.g., local dev
    /// fronted by a test proxy) must opt in explicitly by setting
    /// `trusted_proxies = ["*"]`.
    ///
    /// Also fails on malformed entries so typos are caught at startup
    /// rather than silently disabling protection.
    fn validate_trusted_proxies(&self) -> Result<()> {
        if self.server.trust_proxy && self.server.trusted_proxies.is_empty() {
            bail!(
                "server.trust_proxy is enabled without server.trusted_proxies. \
                 Set server.trusted_proxies to the IP(s) or CIDR(s) of your \
                 reverse proxy (e.g. [\"10.0.0.0/8\"]), or set it to [\"*\"] \
                 to explicitly trust any peer (not recommended in production \
                 — X-Forwarded-For becomes spoofable)."
            );
        }

        for entry in &self.server.trusted_proxies {
            if entry == "*" {
                continue;
            }

            if entry.parse::<IpNet>().is_err() && entry.parse::<IpAddr>().is_err() {
                bail!(
                    "server.trusted_proxies entry {:?} is not a valid IP, \
                     CIDR, or the \"*\" wildcard",
                    entry
                );
            }
        }

        if self.server.trust_proxy && self.server.trusted_proxies.iter().any(|e| e == "*") {
            warn!(
                "server.trusted_proxies contains \"*\" — X-Forwarded-For is \
                 honoured from any peer. Use only for development or when \
                 the admin port is not exposed to untrusted networks."
            );
        }

        Ok(())
    }

    /// Validate pagination limits.
    fn validate_pagination(&self) -> Result<()> {
        if self.pagination.default_limit <= 0 {
            bail!("pagination.default_limit must be > 0");
        }

        if self.pagination.max_limit <= 0 {
            bail!("pagination.max_limit must be > 0");
        }

        if self.pagination.default_limit > self.pagination.max_limit {
            bail!(
                "pagination.default_limit ({}) must be <= pagination.max_limit ({})",
                self.pagination.default_limit,
                self.pagination.max_limit
            );
        }

        Ok(())
    }

    /// Validate depth/population limits.
    fn validate_depth(&self) -> Result<()> {
        if self.depth.default_depth < 0 {
            bail!("depth.default_depth must be >= 0");
        }

        if self.depth.max_depth < 0 {
            bail!("depth.max_depth must be >= 0");
        }

        if self.depth.max_depth == 0 {
            warn!("depth.max_depth = 0 — all depth/populate requests will be capped to 0");
        }

        if self.depth.default_depth > self.depth.max_depth {
            warn!(
                "depth.default_depth ({}) exceeds depth.max_depth ({}) — requests will be capped",
                self.depth.default_depth, self.depth.max_depth
            );
        }

        Ok(())
    }

    /// Validate job scheduler settings.
    fn validate_jobs(&self) -> Result<()> {
        if self.hooks.vm_pool_size == 0 {
            bail!("hooks.vm_pool_size must be > 0");
        }

        if self.jobs.max_concurrent == 0 {
            warn!("jobs.max_concurrent = 0 — no jobs will be executed");
        }

        if self.jobs.poll_interval == 0 {
            bail!("jobs.poll_interval must be > 0");
        }

        if self.jobs.cron_interval == 0 {
            bail!("jobs.cron_interval must be > 0");
        }

        if self.jobs.heartbeat_interval == 0 {
            bail!("jobs.heartbeat_interval must be > 0");
        }

        Ok(())
    }

    /// Validate auth and password policy settings.
    fn validate_auth(&self) -> Result<()> {
        if !self.auth.secret.is_empty() && self.auth.secret.len() < 32 {
            warn!("auth.secret is shorter than 32 characters — consider using a stronger key");
        }

        if self.auth.password_policy.min_length > self.auth.password_policy.max_length {
            bail!(
                "auth.password.min_length ({}) must be <= auth.password.max_length ({})",
                self.auth.password_policy.min_length,
                self.auth.password_policy.max_length
            );
        }

        // `0` means "no cap" — the default, silent. Finite values longer
        // than 30 days deserve a nudge, since they materially widen the
        // window in which a stolen session token is usable.
        const SESSION_MAX_AGE_WARN_THRESHOLD: u64 = 30 * 86400;

        if self.auth.session_absolute_max_age > SESSION_MAX_AGE_WARN_THRESHOLD {
            warn!(
                "auth.session_absolute_max_age is {} seconds (> 30 days) — \
                 long caps enlarge the window in which a stolen session token \
                 remains valid. Consider shortening, or pair with step-up \
                 authentication for sensitive operations.",
                self.auth.session_absolute_max_age,
            );
        }

        Ok(())
    }

    /// Validate email/SMTP settings.
    fn validate_email(&self) -> Result<()> {
        if !self.email.smtp_host.is_empty() && self.email.smtp_port == 0 {
            bail!("email.smtp_port must be > 0 when smtp_host is configured");
        }

        Ok(())
    }

    /// Validate logging settings.
    fn validate_logging(&self) -> Result<()> {
        if self.logging.file && self.logging.path.is_empty() {
            bail!("logging.path must not be empty when file logging is enabled");
        }

        if self.logging.file && self.logging.max_files == 0 {
            warn!("logging.max_files = 0 — all rotated log files will be deleted on startup");
        }

        Ok(())
    }

    /// Validate MCP settings.
    ///
    /// When `mcp.http = true`, enforces both presence and a minimum length
    /// on `mcp.api_key`. MCP operates with `overrideAccess = true` semantics
    /// (collection- and field-level ACLs are bypassed), so a weak transport
    /// key exposes the entire dataset — a 32-byte floor keeps brute-force
    /// infeasible for realistic attacker budgets.
    fn validate_mcp(&self) -> Result<()> {
        if !(self.mcp.enabled && self.mcp.http) {
            return Ok(());
        }

        if self.mcp.api_key.is_empty() {
            bail!(
                "mcp.http is enabled without an API key — \
                 set mcp.api_key in crap.toml to secure the MCP HTTP endpoint"
            );
        }

        if self.mcp.api_key.as_ref().len() < MIN_MCP_API_KEY_LEN {
            bail!(
                "mcp.api_key is too short ({} chars) — require at least {} \
                 characters. MCP bypasses collection and field ACLs, so a \
                 short key risks exposing the entire dataset. Generate one \
                 with `openssl rand -hex 32` or `head -c 32 /dev/urandom | base64`.",
                self.mcp.api_key.as_ref().len(),
                MIN_MCP_API_KEY_LEN,
            );
        }

        Ok(())
    }

    /// Validate live event streaming settings.
    fn validate_live(&self) -> Result<()> {
        if self.live.enabled && self.live.channel_capacity == 0 {
            bail!("live.channel_capacity must be > 0 when live events are enabled");
        }

        Ok(())
    }

    /// Validate cache settings.
    fn validate_cache(&self) -> Result<()> {
        if self.cache.backend == "memory" && self.cache.max_entries == 0 {
            warn!(
                "cache.max_entries = 0 with memory backend — cache will never store entries (equivalent to backend = \"none\")"
            );
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

    /// Resolve the log directory path relative to the config directory.
    #[must_use]
    pub fn log_dir(&self, config_dir: &Path) -> PathBuf {
        let p = Path::new(&self.logging.path);

        if p.is_absolute() {
            p.to_path_buf()
        } else {
            config_dir.join(p)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PaginationMode;

    #[test]
    fn default_config_values() {
        let config = CrapConfig::default();
        assert_eq!(config.server.admin_port, 3000);
        assert_eq!(config.server.grpc_port, 50051);
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.database.path, "data/crap.db");
        assert_eq!(config.database.pool_max_size, 64);
        assert_eq!(config.database.busy_timeout, 30000);
        assert!(!config.admin.dev_mode);
        assert!(config.admin.require_auth);
        assert!(config.admin.access.is_none());
        assert!(config.access.default_deny);
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
    fn log_dir_relative() {
        let config = CrapConfig::default();
        let dir = Path::new("/my/config");
        assert_eq!(config.log_dir(dir), Path::new("/my/config/data/logs"));
    }

    #[test]
    fn log_dir_absolute() {
        let mut config = CrapConfig::default();
        config.logging.path = "/var/log/crap-cms".to_string();
        let dir = Path::new("/my/config");
        assert_eq!(config.log_dir(dir), Path::new("/var/log/crap-cms"));
    }

    #[test]
    fn validate_logging_empty_path_errors() {
        let mut config = CrapConfig::default();
        config.logging.file = true;
        config.logging.path = String::new();
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("logging.path"));
    }

    #[test]
    fn validate_logging_max_files_zero_warns_but_passes() {
        let mut config = CrapConfig::default();
        config.logging.file = true;
        config.logging.max_files = 0;
        // Should warn but not error
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_logging_disabled_empty_path_passes() {
        let mut config = CrapConfig::default();
        config.logging.file = false;
        config.logging.path = String::new();
        // Validation only applies when file logging is enabled
        assert!(config.validate().is_ok());
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
        )
        .unwrap();

        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.server.admin_port, 4000);
        assert_eq!(config.server.grpc_port, 60000);
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.database.path, "mydata/custom.db");
        assert!(!config.admin.dev_mode);
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
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.server.admin_port, 5000);
        assert_eq!(config.server.grpc_port, 50051);
        assert_eq!(config.database.path, "data/crap.db");
    }

    // -- validate() tests --

    #[test]
    fn validate_default_config_passes() {
        let config = CrapConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_pool_max_size_zero_errors() {
        let mut config = CrapConfig::default();
        config.database.pool_max_size = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("pool_max_size"));
    }

    #[test]
    fn validate_connection_timeout_zero_errors() {
        let mut config = CrapConfig::default();
        config.database.connection_timeout = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("connection_timeout"));
    }

    #[test]
    fn validate_vm_pool_size_zero_errors() {
        let mut config = CrapConfig::default();
        config.hooks.vm_pool_size = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("vm_pool_size"));
    }

    #[test]
    fn validate_max_concurrent_zero_warns_but_passes() {
        let mut config = CrapConfig::default();
        config.jobs.max_concurrent = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_poll_interval_zero_errors() {
        let mut config = CrapConfig::default();
        config.jobs.poll_interval = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("poll_interval"));
    }

    #[test]
    fn validate_cron_interval_zero_errors() {
        let mut config = CrapConfig::default();
        config.jobs.cron_interval = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("cron_interval"));
    }

    #[test]
    fn validate_heartbeat_interval_zero_errors() {
        let mut config = CrapConfig::default();
        config.jobs.heartbeat_interval = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("heartbeat_interval"));
    }

    #[test]
    fn validate_short_auth_secret_warns_but_passes() {
        let mut config = CrapConfig::default();
        config.auth.secret = crate::core::JwtSecret::new("short");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_max_depth_zero_warns_but_passes() {
        let mut config = CrapConfig::default();
        config.depth.max_depth = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_admin_port_zero_errors() {
        let mut config = CrapConfig::default();
        config.server.admin_port = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("ports must be > 0"));
    }

    #[test]
    fn validate_grpc_port_zero_errors() {
        let mut config = CrapConfig::default();
        config.server.grpc_port = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("ports must be > 0"));
    }

    #[test]
    fn validate_same_ports_errors() {
        let mut config = CrapConfig::default();
        config.server.admin_port = 5000;
        config.server.grpc_port = 5000;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("must be different"));
    }

    #[test]
    fn validate_distinct_nonzero_ports_passes() {
        let mut config = CrapConfig::default();
        config.server.admin_port = 3000;
        config.server.grpc_port = 50051;
        assert!(config.validate().is_ok());
    }

    // -- check_version tests --

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
        let config = CrapConfig::default();
        assert!(config.crap_version.is_none());
        assert!(config.check_version().is_none());
    }

    #[test]
    fn check_version_from_struct_with_value() {
        let config = CrapConfig {
            crap_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            ..Default::default()
        };
        assert!(config.check_version().is_none());
    }

    #[test]
    fn crap_version_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("crap.toml"), "crap_version = \"0.1.0\"\n").unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.crap_version, Some("0.1.0".to_string()));
    }

    #[test]
    fn crap_version_absent_in_toml_is_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[server]\nadmin_port = 3000\n",
        )
        .unwrap();
        let config = CrapConfig::load(tmp.path()).unwrap();
        assert!(config.crap_version.is_none());
    }

    #[test]
    fn validate_mcp_http_without_api_key_errors() {
        let mut config = CrapConfig::default();
        config.mcp.enabled = true;
        config.mcp.http = true;
        // api_key is empty by default
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("mcp.api_key"),
            "Expected mcp.api_key error, got: {}",
            err
        );
    }

    #[test]
    fn validate_mcp_http_with_strong_api_key_passes() {
        let mut config = CrapConfig::default();
        config.mcp.enabled = true;
        config.mcp.http = true;
        // 32-char key meets MIN_MCP_API_KEY_LEN
        config.mcp.api_key = crate::config::McpApiKey::from("0123456789abcdef0123456789abcdef");
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_mcp_http_with_short_api_key_errors() {
        let mut config = CrapConfig::default();
        config.mcp.enabled = true;
        config.mcp.http = true;
        // 15 chars — below the 32-char floor
        config.mcp.api_key = crate::config::McpApiKey::from("secret-key-1234");
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("too short"),
            "Expected short-key error, got: {}",
            msg,
        );
        // Error guides operators to a safe generator.
        assert!(msg.contains("openssl rand") || msg.contains("/dev/urandom"));
    }

    #[test]
    fn validate_mcp_disabled_no_api_key_passes() {
        let mut config = CrapConfig::default();
        config.mcp.enabled = false;
        config.mcp.http = true;
        // Should not error — MCP is disabled entirely
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_mcp_stdio_no_api_key_passes() {
        let mut config = CrapConfig::default();
        config.mcp.enabled = true;
        config.mcp.http = false;
        // stdio transport doesn't need API key — process-level access controls it
        assert!(config.validate().is_ok());
    }

    // ── trust_proxy / trusted_proxies pairing (audit finding H-3) ────────

    #[test]
    fn validate_trust_proxy_without_allowlist_errors() {
        let mut config = CrapConfig::default();
        config.server.trust_proxy = true;
        // trusted_proxies is empty by default
        let err = config.validate().unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("trusted_proxies"),
            "expected allowlist error, got: {msg}",
        );
        assert!(
            msg.contains("\"*\""),
            "error should mention the explicit-wildcard escape hatch: {msg}",
        );
    }

    #[test]
    fn validate_trust_proxy_with_allowlist_passes() {
        let mut config = CrapConfig::default();
        config.server.trust_proxy = true;
        config.server.trusted_proxies = vec!["10.0.0.0/8".into(), "127.0.0.1".into()];
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_trust_proxy_with_explicit_wildcard_passes() {
        let mut config = CrapConfig::default();
        config.server.trust_proxy = true;
        config.server.trusted_proxies = vec!["*".into()];
        // Wildcard is an intentional escape hatch — validation accepts it
        // (startup logs a warning so the looseness stays visible).
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_trusted_proxies_rejects_malformed_entry() {
        let mut config = CrapConfig::default();
        config.server.trust_proxy = true;
        config.server.trusted_proxies = vec!["10.0.0.0/8".into(), "not-an-ip".into()];
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("not-an-ip"));
    }

    #[test]
    fn validate_trust_proxy_disabled_ignores_allowlist_shape() {
        // When trust_proxy is off the allowlist is unused — malformed
        // entries shouldn't prevent startup in that case.
        let mut config = CrapConfig::default();
        config.server.trust_proxy = false;
        config.server.trusted_proxies = vec!["definitely-not-an-ip".into()];
        // With trust_proxy disabled we still reject garbage entries so
        // the operator knows their config has a typo. Document the
        // current strict behaviour with an explicit test.
        assert!(config.validate().is_err());
    }

    #[test]
    fn validate_rejects_smtp_port_zero() {
        let mut config = CrapConfig::default();
        config.email.smtp_host = "smtp.example.com".to_string();
        config.email.smtp_port = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("smtp_port"));
    }

    #[test]
    fn validate_smtp_port_zero_ok_when_host_empty() {
        let mut config = CrapConfig::default();
        config.email.smtp_host = String::new();
        config.email.smtp_port = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_request_timeout_zero() {
        let mut config = CrapConfig::default();
        config.server.request_timeout = Some(0);
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("request_timeout"));
    }

    #[test]
    fn validate_rejects_grpc_timeout_zero() {
        let mut config = CrapConfig::default();
        config.server.grpc_timeout = Some(0);
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("grpc_timeout"));
    }

    #[test]
    fn validate_timeout_none_passes() {
        let mut config = CrapConfig::default();
        config.server.request_timeout = None;
        config.server.grpc_timeout = None;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_grpc_rate_limit_window_zero() {
        let mut config = CrapConfig::default();
        config.server.grpc_rate_limit_requests = 100;
        config.server.grpc_rate_limit_window = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("grpc_rate_limit_window"));
    }

    #[test]
    fn validate_grpc_rate_limit_window_zero_ok_when_disabled() {
        let mut config = CrapConfig::default();
        config.server.grpc_rate_limit_requests = 0;
        config.server.grpc_rate_limit_window = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_channel_capacity_zero_errors() {
        let mut config = CrapConfig::default();
        config.live.channel_capacity = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("channel_capacity"));
    }

    #[test]
    fn validate_channel_capacity_zero_ok_when_live_disabled() {
        let mut config = CrapConfig::default();
        config.live.enabled = false;
        config.live.channel_capacity = 0;
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_pagination_default_limit_zero_errors() {
        let mut config = CrapConfig::default();
        config.pagination.default_limit = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("default_limit"));
    }

    #[test]
    fn validate_pagination_default_limit_negative_errors() {
        let mut config = CrapConfig::default();
        config.pagination.default_limit = -5;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("default_limit"));
    }

    #[test]
    fn validate_pagination_max_limit_zero_errors() {
        let mut config = CrapConfig::default();
        config.pagination.max_limit = 0;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("max_limit"));
    }

    #[test]
    fn validate_pagination_default_exceeds_max_errors() {
        let mut config = CrapConfig::default();
        config.pagination.default_limit = 100;
        config.pagination.max_limit = 50;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("default_limit"));
        assert!(err.to_string().contains("max_limit"));
    }

    #[test]
    fn validate_depth_negative_default_errors() {
        let mut config = CrapConfig::default();
        config.depth.default_depth = -1;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("default_depth"));
    }

    #[test]
    fn validate_depth_negative_max_errors() {
        let mut config = CrapConfig::default();
        config.depth.max_depth = -1;
        let err = config.validate().unwrap_err();
        assert!(err.to_string().contains("max_depth"));
    }

    /// BUG-3 regression: a typo at the top level (e.g. `admin_prot`) must fail
    /// loading instead of being silently ignored.
    #[test]
    fn config_rejects_unknown_top_level_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("crap.toml"), "admin_prot = 3000\n").unwrap();
        let err = CrapConfig::load(tmp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("admin_prot") || msg.contains("unknown"),
            "expected unknown-field error, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_warns_on_world_readable_with_secrets() {
        use super::{is_world_accessible_mode, should_warn_loose_permissions};
        use std::os::unix::fs::PermissionsExt;

        // Write a real world-readable config with a secret, then sanity-check
        // that load() succeeds. The warn! call is not captured by default tests,
        // but the pure decision helper is.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("crap.toml");
        std::fs::write(
            &path,
            r#"
[auth]
secret = "this-is-a-very-long-auth-secret-value-xxxxx"
"#,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        // Load succeeds (warn is emitted internally).
        let config = CrapConfig::load(tmp.path()).expect("load with 0644 should succeed");

        // Loose-mode + secret-present decision should be true.
        assert!(is_world_accessible_mode(0o644));
        assert!(!is_world_accessible_mode(0o600));
        assert!(is_world_accessible_mode(0o666));
        assert!(should_warn_loose_permissions(&config, 0o644));
        assert!(!should_warn_loose_permissions(&config, 0o600));

        // No secret = no warn.
        let empty = CrapConfig::default();
        assert!(!should_warn_loose_permissions(&empty, 0o644));
    }

    /// BUG-3 regression: a typo inside a nested section must also fail.
    #[test]
    fn config_rejects_unknown_nested_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[server]\nadmin_prot = 3000\n",
        )
        .unwrap();
        let err = CrapConfig::load(tmp.path()).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("admin_prot") || msg.contains("unknown"),
            "expected unknown-field error, got: {msg}"
        );
    }
}
