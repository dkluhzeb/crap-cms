//! Configuration types loaded from `crap.toml`.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Top-level configuration loaded from `crap.toml` in the config directory.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct CrapConfig {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub admin: AdminConfig,
    pub hooks: HooksConfig,
    pub auth: AuthConfig,
    pub depth: DepthConfig,
    pub upload: UploadConfig,
}

/// Controls relationship population depth defaults and limits.
#[derive(Debug, Clone, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
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

/// Hook configuration — currently just `on_init` script references.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct HooksConfig {
    pub on_init: Vec<String>,
}

/// JWT authentication settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// JWT secret. If empty, a random secret is generated at startup (tokens
    /// won't survive restarts).
    pub secret: String,
    /// Default token expiry in seconds (can be overridden per-collection).
    pub token_expiry: u64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            secret: String::new(),
            token_expiry: 7200,
        }
    }
}

/// Admin UI and gRPC server bind settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub admin_port: u16,
    pub grpc_port: u16,
    pub host: String,
}

/// SQLite database path configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub path: String,
}

/// Admin UI behavior settings.
#[derive(Debug, Clone, Deserialize)]
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
            dev_mode: true,
        }
    }
}

impl CrapConfig {
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
        assert!(config.admin.dev_mode);
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
}
