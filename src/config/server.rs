//! Server, database, and admin configuration structs.

use serde::{Deserialize, Serialize};

use super::parsing::{serde_duration, serde_duration_ms};

/// Response compression mode for the admin HTTP server.
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CompressionMode {
    /// Disable compression (default).
    #[default]
    Off,
    /// Enable Gzip compression.
    Gzip,
    /// Enable Brotli compression.
    Br,
    /// Enable all supported compression modes.
    All,
}

/// Admin UI and gRPC server bind settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Port for the admin UI HTTP server. Default: 3000.
    pub admin_port: u16,
    /// Port for the gRPC API server. Default: 50051.
    pub grpc_port: u16,
    /// Host interface to bind to. Default: "0.0.0.0".
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

/// SQLite database path and pool configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DatabaseConfig {
    /// Path to the SQLite database file.
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

/// Admin UI behavior settings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AdminConfig {
    /// Enable development mode (e.g., more verbose errors).
    pub dev_mode: bool,
    /// When true (default), block admin panel if no auth collection exists.
    /// Set to false for open dev mode with no authentication.
    pub require_auth: bool,
    /// Optional Lua function ref that gates admin panel access.
    /// Checked after successful authentication. None = any authenticated user.
    pub access: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

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
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert!(config.admin.dev_mode);
        assert!(!config.admin.require_auth);
        assert_eq!(config.admin.access, Some("access.admin_panel".to_string()));
    }

    #[test]
    fn admin_config_partial_toml_uses_defaults() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("crap.toml"), "[admin]\ndev_mode = true\n").unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert!(config.admin.dev_mode);
        assert!(config.admin.require_auth); // default
        assert!(config.admin.access.is_none()); // default
    }

    #[test]
    fn database_config_from_toml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("crap.toml"),
            "[database]\npool_max_size = 32\nbusy_timeout = 60000\n",
        )
        .unwrap();
        let config = crate::config::CrapConfig::load(tmp.path()).unwrap();
        assert_eq!(config.database.pool_max_size, 32);
        assert_eq!(config.database.busy_timeout, 60000);
    }
}
