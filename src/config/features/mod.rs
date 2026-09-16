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

#[cfg(test)]
pub(crate) use jobs::JOB_DRAIN_GRACE_SECS;
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
pub(crate) use jobs::{
    DEFAULT_BULK_QUEUE_TIMEOUT_SECS, DEFAULT_EMAIL_QUEUE_TIMEOUT_SECS,
    DEFAULT_IMAGES_QUEUE_TIMEOUT_SECS, SELF_LIMITING_JOB_GRACE_SECS,
};
pub use jobs::{JobsConfig, QueueConfig};
pub use live::{LiveConfig, LiveTransport};
pub use locale::LocaleConfig;
pub use logging::{LogRotation, LoggingConfig};
pub use mcp::{McpConfig, McpJobTools};
pub use pagination::{PaginationConfig, PaginationMode};
pub use update::UpdateConfig;
pub use upload::{S3Config, UploadConfig, UploadStorage};
