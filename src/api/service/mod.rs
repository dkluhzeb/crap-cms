//! Tonic gRPC service implementing all ContentAPI RPCs.

mod auth;
mod collection;
mod convert;
mod deps_builder;
mod schema_ops;
mod service_impl;

pub use deps_builder::ContentServiceDepsBuilder;
pub use service_impl::ContentService;

use std::path::PathBuf;
use std::sync::Arc;

use crate::{
    config::CrapConfig,
    core::{
        JwtSecret, Registry,
        auth::{SharedPasswordProvider, SharedTokenProvider},
        cache::SharedCache,
        email::EmailRenderer,
        event::EventBus,
        rate_limit::LoginRateLimiter,
        upload::SharedStorage,
    },
    db::DbPool,
    hooks::HookRunner,
};

/// Dependencies for constructing a `ContentService`.
pub struct ContentServiceDeps {
    pub pool: DbPool,
    pub registry: Arc<Registry>,
    pub hook_runner: HookRunner,
    pub jwt_secret: JwtSecret,
    pub config: CrapConfig,
    pub config_dir: PathBuf,
    pub email_renderer: Arc<EmailRenderer>,
    pub event_bus: Option<EventBus>,
    pub login_limiter: Arc<LoginRateLimiter>,
    pub ip_login_limiter: Arc<LoginRateLimiter>,
    pub forgot_password_limiter: Arc<LoginRateLimiter>,
    pub ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    pub storage: SharedStorage,
    pub cache: SharedCache,
    pub token_provider: SharedTokenProvider,
    pub password_provider: SharedPasswordProvider,
}

impl ContentServiceDeps {
    /// Create a builder for `ContentServiceDeps`.
    pub fn builder() -> ContentServiceDepsBuilder {
        ContentServiceDepsBuilder::new()
    }
}
