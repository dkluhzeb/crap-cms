//! Configuration types loaded from `crap.toml`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level configuration loaded from `crap.toml` in the config directory.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct CrapConfig {
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
}

impl Default for DepthConfig {
    fn default() -> Self {
        Self {
            default_depth: 1,
            max_depth: 10,
        }
    }
}

/// Global upload settings (per-collection upload config is separate).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct UploadConfig {
    /// Global max file size in bytes. Default: 50MB.
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
    /// How often to poll for pending jobs, in seconds. Default: 1.
    pub poll_interval: u64,
    /// How often to check cron schedules, in seconds. Default: 60.
    pub cron_interval: u64,
    /// How often to update heartbeat for running jobs, in seconds. Default: 10.
    pub heartbeat_interval: u64,
    /// Auto-purge completed/failed jobs older than this duration string (e.g., "7d").
    /// Empty string disables auto-purge.
    pub auto_purge: String,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            max_concurrent: 10,
            poll_interval: 1,
            cron_interval: 60,
            heartbeat_interval: 10,
            auto_purge: "7d".to_string(),
        }
    }
}

impl JobsConfig {
    /// Parse the `auto_purge` duration string into seconds.
    /// Supports "Nd" (days), "Nh" (hours), "Nm" (minutes). Returns None if empty or invalid.
    pub fn auto_purge_seconds(&self) -> Option<u64> {
        let s = self.auto_purge.trim();
        if s.is_empty() {
            return None;
        }
        let (num_str, suffix) = s.split_at(s.len().saturating_sub(1));
        let num: u64 = num_str.parse().ok()?;
        match suffix {
            "d" => Some(num * 86400),
            "h" => Some(num * 3600),
            "m" => Some(num * 60),
            _ => None,
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
    /// Number of Lua VMs in the hook runner pool. Default: min(available_parallelism, 8).
    /// Higher values allow more concurrent hook execution.
    pub vm_pool_size: usize,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            on_init: Vec::new(),
            max_depth: 3,
            vm_pool_size: std::thread::available_parallelism()
                .map(|n| n.get().min(8))
                .unwrap_or(4),
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
    pub token_expiry: u64,
    /// Max failed login attempts before lockout. Default: 5.
    pub max_login_attempts: u32,
    /// Lockout window in seconds. Default: 300 (5 minutes).
    pub login_lockout_seconds: u64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            secret: String::new(),
            token_expiry: 7200,
            max_login_attempts: 5,
            login_lockout_seconds: 300,
        }
    }
}

/// Admin UI and gRPC server bind settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    pub admin_port: u16,
    pub grpc_port: u16,
    pub host: String,
}

/// SQLite database path configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub path: String,
}

/// Admin UI behavior settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AdminConfig {
    pub dev_mode: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            admin_port: 3000,
            grpc_port: 50051,
            host: "0.0.0.0".to_string(),
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: "data/crap.db".to_string(),
        }
    }
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            dev_mode: false,
        }
    }
}

impl CrapConfig {
    /// Load configuration from `crap.toml` in the config directory, falling back to defaults.
    pub fn load(config_dir: &Path) -> Result<Self> {
        let config_path = config_dir.join("crap.toml");
        if config_path.exists() {
            let contents = std::fs::read_to_string(&config_path)
                .with_context(|| format!("Failed to read {}", config_path.display()))?;
            let config: CrapConfig = toml::from_str(&contents)
                .with_context(|| format!("Failed to parse {}", config_path.display()))?;
            Ok(config)
        } else {
            tracing::info!("No crap.toml found, using defaults");
            Ok(CrapConfig::default())
        }
    }

    /// Resolve the database path relative to the config directory.
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

    #[test]
    fn default_config_values() {
        let config = CrapConfig::default();
        assert_eq!(config.server.admin_port, 3000);
        assert_eq!(config.server.grpc_port, 50051);
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.database.path, "data/crap.db");
        assert!(!config.admin.dev_mode);
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

    #[test]
    fn auto_purge_seconds_days() {
        let mut cfg = JobsConfig::default();
        cfg.auto_purge = "7d".to_string();
        assert_eq!(cfg.auto_purge_seconds(), Some(7 * 86400));
    }

    #[test]
    fn auto_purge_seconds_hours() {
        let mut cfg = JobsConfig::default();
        cfg.auto_purge = "24h".to_string();
        assert_eq!(cfg.auto_purge_seconds(), Some(24 * 3600));
    }

    #[test]
    fn auto_purge_seconds_minutes() {
        let mut cfg = JobsConfig::default();
        cfg.auto_purge = "30m".to_string();
        assert_eq!(cfg.auto_purge_seconds(), Some(30 * 60));
    }

    #[test]
    fn auto_purge_seconds_empty() {
        let mut cfg = JobsConfig::default();
        cfg.auto_purge = "".to_string();
        assert_eq!(cfg.auto_purge_seconds(), None);
    }

    #[test]
    fn auto_purge_seconds_invalid() {
        let mut cfg = JobsConfig::default();
        cfg.auto_purge = "7s".to_string();
        assert_eq!(cfg.auto_purge_seconds(), None);
    }

    #[test]
    fn auto_purge_seconds_default_config() {
        let cfg = JobsConfig::default();
        assert_eq!(cfg.auto_purge, "7d");
        assert_eq!(cfg.auto_purge_seconds(), Some(7 * 86400));
    }

    #[test]
    fn jobs_config_defaults() {
        let cfg = JobsConfig::default();
        assert_eq!(cfg.max_concurrent, 10);
        assert_eq!(cfg.poll_interval, 1);
        assert_eq!(cfg.cron_interval, 60);
        assert_eq!(cfg.heartbeat_interval, 10);
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
    }

    #[test]
    fn hooks_config_defaults() {
        let hooks = HooksConfig::default();
        assert!(hooks.on_init.is_empty());
        assert_eq!(hooks.max_depth, 3);
        assert!(hooks.vm_pool_size >= 1 && hooks.vm_pool_size <= 8);
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
}
