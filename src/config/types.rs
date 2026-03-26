//! Top-level `CrapConfig` struct and its loading/validation logic.

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

use super::{
    auth::AuthConfig,
    cors::CorsConfig,
    env::substitute_in_value,
    features::{
        AccessConfig, DepthConfig, EmailConfig, HooksConfig, JobsConfig, LiveConfig, LocaleConfig,
        McpConfig, PaginationConfig, UploadConfig,
    },
    server::{AdminConfig, DatabaseConfig, ServerConfig},
};

/// Top-level configuration loaded from `crap.toml` in the config directory.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
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
            bail!("database.pool_max_size must be > 0");
        }

        // Fatal: instant connection timeout
        if self.database.connection_timeout == 0 {
            bail!("database.connection_timeout must be > 0");
        }

        // Fatal: Lua VM pool with no VMs
        if self.hooks.vm_pool_size == 0 {
            bail!("hooks.vm_pool_size must be > 0");
        }

        // Warning: no jobs will execute
        if self.jobs.max_concurrent == 0 {
            tracing::warn!("jobs.max_concurrent = 0 — no jobs will be executed");
        }

        // Warning: weak JWT signing key (when explicitly set)
        if !self.auth.secret.is_empty() && self.auth.secret.len() < 32 {
            tracing::warn!(
                "auth.secret is shorter than 32 characters — consider using a stronger key"
            );
        }

        // Fatal: port 0 is not a valid listen port
        if self.server.admin_port == 0 || self.server.grpc_port == 0 {
            bail!("Server ports must be > 0");
        }

        // Fatal: ports must be distinct
        if self.server.admin_port == self.server.grpc_port {
            bail!("admin_port and grpc_port must be different");
        }

        // Fatal: broadcast channel capacity must be > 0 (tokio panics on 0)
        if self.live.enabled && self.live.channel_capacity == 0 {
            bail!("live.channel_capacity must be > 0 when live events are enabled");
        }

        // Fatal: pagination limits must be positive
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

        // Fatal: negative depth values make no sense
        if self.depth.default_depth < 0 {
            bail!("depth.default_depth must be >= 0");
        }
        if self.depth.max_depth < 0 {
            bail!("depth.max_depth must be >= 0");
        }

        // Warning: max_depth = 0 means no population will ever work
        if self.depth.max_depth == 0 {
            tracing::warn!("depth.max_depth = 0 — all depth/populate requests will be capped to 0");
        }

        // Warning: default_depth exceeds max_depth
        if self.depth.default_depth > self.depth.max_depth {
            tracing::warn!(
                "depth.default_depth ({}) exceeds depth.max_depth ({}) — requests will be capped",
                self.depth.default_depth,
                self.depth.max_depth
            );
        }

        // Fatal: MCP HTTP without API key is unauthenticated full CRUD access
        if self.mcp.enabled && self.mcp.http && self.mcp.api_key.is_empty() {
            bail!(
                "mcp.http is enabled without an API key — \
                 set mcp.api_key in crap.toml to secure the MCP HTTP endpoint"
            );
        }

        // Fatal: password min_length > max_length
        if self.auth.password_policy.min_length > self.auth.password_policy.max_length {
            bail!(
                "auth.password.min_length ({}) must be <= auth.password.max_length ({})",
                self.auth.password_policy.min_length,
                self.auth.password_policy.max_length
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
    fn validate_mcp_http_with_api_key_passes() {
        let mut config = CrapConfig::default();
        config.mcp.enabled = true;
        config.mcp.http = true;
        config.mcp.api_key = crate::config::McpApiKey::from("secret-key-123");
        assert!(config.validate().is_ok());
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
}
