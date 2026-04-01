//! Admin UI: Axum server, Handlebars templates, and HTMX-powered handlers.

pub mod context;
pub mod context_builder;
pub mod handlers;
pub mod server;
pub mod server_builder;
pub mod templates;
pub mod translations;

pub use context_builder::ContextBuilder;
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
        JwtSecret, Registry, email::EmailRenderer, email::SharedEmailProvider, event::EventBus,
        rate_limit::LoginRateLimiter, upload::SharedStorage,
    },
    db::DbPool,
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
    /// The event bus for asynchronous event handling, if enabled.
    pub event_bus: Option<EventBus>,
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
    /// Pre-computed Content-Security-Policy header value. None = CSP disabled.
    pub csp_header: Option<String>,
    /// The storage backend for uploaded files.
    pub storage: SharedStorage,
}

impl AdminState {
    /// Render a template with the given data, returning HTML string.
    pub fn render(&self, template: &str, data: &Value) -> Result<String, String> {
        self.handlebars
            .render(template, data)
            .map_err(|e| format!("Template error: {}", e))
    }
}
