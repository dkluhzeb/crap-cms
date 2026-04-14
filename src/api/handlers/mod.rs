//! Tonic gRPC service implementing all ContentAPI RPCs.

use std::{path::PathBuf, sync::Arc};

mod auth;
mod collection;
mod content_service;
mod convert;
mod deps_builder;
mod globals;
mod jobs;
mod schema;
mod subscribe;

pub use content_service::ContentService;
pub use deps_builder::ContentServiceDepsBuilder;

use crate::{
    config::CrapConfig,
    core::{
        JwtSecret, Registry,
        auth::{SharedPasswordProvider, SharedTokenProvider},
        cache::SharedCache,
        email::EmailRenderer,
        event::{SharedEventTransport, SharedInvalidationTransport},
        rate_limit::LoginRateLimiter,
        upload::SharedStorage,
    },
    db::{DbPool, query::SharedPopulateSingleflight},
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
    pub event_transport: Option<SharedEventTransport>,
    pub login_limiter: Arc<LoginRateLimiter>,
    pub ip_login_limiter: Arc<LoginRateLimiter>,
    pub forgot_password_limiter: Arc<LoginRateLimiter>,
    pub ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    pub storage: SharedStorage,
    pub cache: SharedCache,
    pub token_provider: SharedTokenProvider,
    pub password_provider: SharedPasswordProvider,
    /// Optional: shared invalidation transport. When `None`, a fresh
    /// in-process one is created internally.
    pub invalidation_transport: Option<SharedInvalidationTransport>,
    /// Optional: shared populate singleflight. When `None`, a fresh
    /// process-wide one is created internally for this service.
    pub populate_singleflight: Option<SharedPopulateSingleflight>,
}

impl ContentServiceDeps {
    /// Create a builder for `ContentServiceDeps`.
    pub fn builder() -> ContentServiceDepsBuilder {
        ContentServiceDepsBuilder::new()
    }
}
