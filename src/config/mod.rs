//! Configuration types loaded from `crap.toml`. The user-facing API is
//! the flat `crate::config::*` re-export block at the bottom; submodules
//! are `pub(crate)` so internal callers can still reach them through the
//! short path but external paths flatten to one level.
//!
//! ## Submodule layout
//!
//! - `types.rs` -- top-level `CrapConfig`: load, defaults, version check,
//!   path helpers, and the `validate()` orchestrator.
//! - `validate.rs` -- per-section `validate_*` methods on `CrapConfig`,
//!   each isolated for tight test coverage.
//! - `features/` -- one file per `[<section>]` table in `crap.toml`:
//!   email, depth, cache, pagination, MCP, upload, locale, jobs, live,
//!   hooks, access, logging, update.
//! - `server/` -- `ServerConfig`, `DatabaseConfig`, `AdminConfig`, and
//!   the admin-only `CspConfig`.
//! - `auth/` -- `AuthConfig`, `PasswordPolicy`, and
//!   `SessionCookieSameSite`.
//! - `cors.rs`, `env.rs`, `parsing.rs` -- single-purpose siblings.
//! - `mcp_api_key.rs`, `s3_secret_key.rs`, `smtp_password.rs` --
//!   redacted-on-Debug newtype wrappers around secrets so they don't
//!   leak via tracing, JSON dumps, or `crap.config.get` from a hook.
//!
//! ## Conventions
//!
//! - Every section has an `impl Default` reflecting `crap.toml`-omitted
//!   semantics. Manual impls only when defaults are non-trivial.
//! - String <-> duration / filesize parsing routes through
//!   `parsing::serde_*` helpers so `"30s"` and `30` both work.
//! - `#[serde(default, deny_unknown_fields)]` everywhere so a typo in
//!   `crap.toml` fails loading rather than being silently dropped.

mod env;
mod parsing;

mod auth;
mod cors;
mod features;
/// Newtype wrapper for MCP API keys.
pub mod mcp_api_key;
pub mod redis_url;
mod routes;
/// Newtype wrapper for S3 secret access keys.
pub mod s3_secret_key;
mod server;
/// Newtype wrapper for SMTP passwords.
pub mod smtp_password;
mod types;
mod validate;
pub mod webhook_headers;

pub use auth::{AuthConfig, PasswordPolicy, RateLimitBackend, SessionCookieSameSite};
pub use cors::CorsConfig;
pub use features::{
    AccessConfig, CacheBackend, CacheConfig, DepthConfig, EmailConfig, EmailProvider, HooksConfig,
    JobsConfig, LiveConfig, LiveTransport, LocaleConfig, LogRotation, LoggingConfig, McpConfig,
    McpJobTools, PaginationConfig, PaginationMode, QueueConfig, S3Config, SmtpTls, UpdateConfig,
    UploadConfig, UploadStorage,
};
pub(crate) use features::{
    DEFAULT_BULK_QUEUE_TIMEOUT_SECS, DEFAULT_EMAIL_QUEUE_TIMEOUT_SECS,
    DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS,
};
pub use mcp_api_key::McpApiKey;
pub(crate) use parsing::{parse_duration_string, parse_filesize_string};
pub use redis_url::{DbUrl, RedisUrl};
pub use routes::RoutesConfig;
pub use s3_secret_key::S3SecretKey;
pub use server::{
    AdminConfig, CompressionMode, CspConfig, DatabaseBackend, DatabaseConfig, ServerConfig,
};
pub use smtp_password::SmtpPassword;
pub use types::{ConfigKeys, CrapConfig};
pub use webhook_headers::WebhookHeaders;
