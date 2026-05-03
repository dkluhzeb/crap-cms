//! Configuration types loaded from `crap.toml`.

mod env;
mod parsing;

mod auth;
mod cors;
mod features;
/// Newtype wrapper for MCP API keys.
pub mod mcp_api_key;
/// Newtype wrapper for S3 secret access keys.
pub mod s3_secret_key;
mod server;
/// Newtype wrapper for SMTP passwords.
pub mod smtp_password;
mod types;

pub use auth::{AuthConfig, PasswordPolicy, SessionCookieSameSite};
pub use cors::CorsConfig;
pub use features::{
    AccessConfig, CacheConfig, DepthConfig, EmailConfig, HooksConfig, JobsConfig, LiveConfig,
    LocaleConfig, LogRotation, LoggingConfig, McpConfig, PaginationConfig, PaginationMode,
    S3Config, SmtpTls, UpdateConfig, UploadConfig,
};
pub use mcp_api_key::McpApiKey;
pub(crate) use parsing::{parse_duration_string, parse_filesize_string};
pub use s3_secret_key::S3SecretKey;
pub use server::{AdminConfig, CompressionMode, DatabaseConfig, ServerConfig};
pub use smtp_password::SmtpPassword;
pub use types::CrapConfig;
