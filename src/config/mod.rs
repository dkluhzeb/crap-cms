//! Configuration types loaded from `crap.toml`.

mod env;
mod parsing;

mod auth;
mod cors;
mod features;
mod server;
/// Newtype wrapper for SMTP passwords.
pub mod smtp_password;
mod types;

pub use auth::{AuthConfig, PasswordPolicy};
pub use cors::CorsConfig;
pub use features::{
    AccessConfig, DepthConfig, EmailConfig, HooksConfig, JobsConfig, LiveConfig, LocaleConfig,
    McpConfig, PaginationConfig, PaginationMode, SmtpTls, UploadConfig,
};
pub(crate) use parsing::{parse_duration_string, parse_filesize_string};
pub use server::{AdminConfig, CompressionMode, DatabaseConfig, ServerConfig};
pub use smtp_password::SmtpPassword;
pub use types::CrapConfig;
