//! Feature configuration sections. One file per `[<section>]` table in
//! `crap.toml`: email, depth, pagination, cache, MCP, upload, locale,
//! jobs, live events, hooks, access, logging, update check.
//!
//! Each section is a self-contained type with its own `Default` impl
//! and colocated tests.

mod access;
mod cache;
mod depth;
mod email;
mod hooks;
mod jobs;
mod live;
mod locale;
mod logging;
mod mcp;
mod pagination;
mod update;
mod upload;

pub use access::AccessConfig;
pub use cache::{CacheBackend, CacheConfig};
pub use depth::DepthConfig;
pub use email::{EmailConfig, EmailProvider, SmtpTls};
pub use hooks::HooksConfig;
pub use jobs::{JobsConfig, QueueConfig};
pub use live::{LiveConfig, LiveTransport};
pub use locale::LocaleConfig;
pub use logging::{LogRotation, LoggingConfig};
pub use mcp::McpConfig;
pub use pagination::{PaginationConfig, PaginationMode};
pub use update::UpdateConfig;
pub use upload::{S3Config, UploadConfig, UploadStorage};
