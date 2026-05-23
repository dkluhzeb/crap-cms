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
/// Newtype wrapper for S3 secret access keys.
pub mod s3_secret_key;
mod server;
/// Newtype wrapper for SMTP passwords.
pub mod smtp_password;
mod types;
mod validate;

pub use auth::{AuthConfig, PasswordPolicy, SessionCookieSameSite};
pub use cors::CorsConfig;
pub use features::{
    AccessConfig, CacheConfig, DepthConfig, EmailConfig, HooksConfig, JobsConfig, LiveConfig,
    LocaleConfig, LogRotation, LoggingConfig, McpConfig, PaginationConfig, PaginationMode,
    QueueConfig, S3Config, SmtpTls, UpdateConfig, UploadConfig,
};
pub use mcp_api_key::McpApiKey;
pub(crate) use parsing::{parse_duration_string, parse_filesize_string};
pub use s3_secret_key::S3SecretKey;
pub use server::{AdminConfig, CompressionMode, DatabaseConfig, ServerConfig};
pub use smtp_password::SmtpPassword;
pub use types::CrapConfig;
