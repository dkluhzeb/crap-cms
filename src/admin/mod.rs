//! Admin UI: Axum server, Handlebars templates, and HTMX-powered handlers.

mod auth_middleware;
pub mod context;
pub mod csp_nonce;
pub mod handlers;
mod mcp_handler;
pub mod server;
pub mod server_builder;
pub mod templates;
pub mod translations;

pub use csp_nonce::{CSP_NONCE, CspNonce, current_nonce_or_empty};
pub use translations::Translations;

use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicUsize},
};

use handlebars::Handlebars;
use tokio_util::sync::CancellationToken;

use serde_json::Value;

use crate::{
    config::CrapConfig,
    core::{
        JwtSecret, Registry,
        auth::{SharedPasswordProvider, SharedTokenProvider},
        cache::SharedCache,
        email::EmailRenderer,
        email::SharedEmailProvider,
        event::{SharedEventTransport, SharedInvalidationTransport},
        rate_limit::LoginRateLimiter,
        upload::SharedStorage,
    },
    db::{DbPool, query::SharedPopulateSingleflight},
    hooks::HookRunner,
};

/// Shared state for all admin handlers.
#[derive(Clone)]
#[allow(dead_code)]
pub struct AdminState {
    /// The global configuration for the CMS.
    pub config: CrapConfig,
    /// The directory where the configuration is located.
    pub config_dir: PathBuf,
    /// The database connection pool.
    pub pool: DbPool,
    /// The registry containing all registered collections and globals.
    pub registry: Arc<Registry>,
    /// The Handlebars template engine instance.
    pub handlebars: Arc<Handlebars<'static>>,
    /// The runner for executing lifecycle hooks.
    pub hook_runner: HookRunner,
    /// The secret key used for signing and verifying JWTs.
    pub jwt_secret: JwtSecret,
    /// The renderer for email notifications.
    pub email_renderer: Arc<EmailRenderer>,
    /// The email provider for sending emails.
    pub email_provider: SharedEmailProvider,
    /// The event transport for live updates (in-process or Redis). None when
    /// live updates are disabled.
    pub event_transport: Option<SharedEventTransport>,
    /// The rate limiter for login attempts (per-email).
    pub login_limiter: Arc<LoginRateLimiter>,
    /// The rate limiter for login attempts (per-IP).
    pub ip_login_limiter: Arc<LoginRateLimiter>,
    /// The rate limiter for password reset requests (per-email).
    pub forgot_password_limiter: Arc<LoginRateLimiter>,
    /// The rate limiter for password reset requests (per-IP).
    pub ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    /// Whether authentication is enabled for the admin UI.
    pub has_auth: bool,
    /// The translations for the admin UI.
    pub translations: Arc<Translations>,
    /// Token used to signal shutdown to the admin server.
    pub shutdown: CancellationToken,
    /// Current number of active SSE connections (for connection limiting).
    pub sse_connections: Arc<AtomicUsize>,
    /// Maximum allowed concurrent SSE connections. 0 = unlimited.
    pub max_sse_connections: usize,
    /// The storage backend for uploaded files.
    pub storage: SharedStorage,
    /// The token provider for JWT creation and validation.
    pub token_provider: SharedTokenProvider,
    /// The password provider for hashing and verification.
    pub password_provider: SharedPasswordProvider,
    /// Per-subscriber SSE send timeout in milliseconds.
    pub subscriber_send_timeout_ms: u64,
    /// Transport for signalling user revocation to active live-update subscribers.
    pub invalidation_transport: SharedInvalidationTransport,
    /// Process-wide singleflight for deduplicating concurrent populate
    /// cache-miss DB fetches across requests. Plumbed into populate contexts
    /// that opt in via the service layer.
    pub populate_singleflight: SharedPopulateSingleflight,
    /// Shared cross-request cache for populated relationship documents.
    /// Passed to service-layer write operations for cache invalidation.
    pub cache: Option<SharedCache>,
}

impl AdminState {
    /// Render a template with the given data, returning HTML string.
    pub fn render(&self, template: &str, data: &Value) -> Result<String, String> {
        self.handlebars
            .render(template, data)
            .map_err(|e| format!("Template error: {}", e))
    }
}
