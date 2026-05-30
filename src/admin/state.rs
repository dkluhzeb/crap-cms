//! `AdminState` — the application-wide bundle handed to every admin
//! handler via `axum::extract::State<AdminState>`. Plus the helper
//! methods that derive sub-bundles (`email_context`, `mcp_server`).

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock, atomic::AtomicUsize},
};

use handlebars::Handlebars;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    config::CrapConfig,
    core::{
        JwtSecret, Registry, SharedCache, SharedEmailProvider, SharedEventTransport,
        SharedInvalidationTransport, SharedPasswordProvider, SharedStorage, SharedTokenProvider,
        email::EmailRenderer, rate_limit::LoginRateLimiter,
    },
    db::{DbPool, query::SharedPopulateSingleflight},
    hooks::HookRunner,
    mcp::McpServer,
    service::EmailContext,
};

use super::{Translations, custom_pages};

/// Shared state for all admin handlers.
#[derive(Clone)]
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
    /// Discovered filesystem-routed custom pages
    /// (`<config_dir>/templates/pages/<slug>.hbs`). Populated once at
    /// startup; the route handler validates incoming slugs against this,
    /// and the sidebar nav reads its `nav_entries`.
    pub custom_pages: custom_pages::CustomPageRegistry,
}

impl AdminState {
    /// Render a template with the given data, returning HTML string.
    ///
    /// # Errors
    ///
    /// Returns a formatted error string if the template is unknown or rendering fails.
    pub fn render(&self, template: &str, data: &Value) -> Result<String, String> {
        self.handlebars
            .render(template, data)
            .map_err(|e| format!("Template error: {e}"))
    }

    /// Bundle the email config + renderer + server config into an
    /// `EmailContext` for verification email flows.
    pub(crate) fn email_context(&self) -> EmailContext {
        EmailContext {
            email_config: self.config.email.clone(),
            email_renderer: self.email_renderer.clone(),
            server_config: self.config.server.clone(),
            email_max_attempts: self.config.jobs.system_email_max_attempts(),
        }
    }

    /// Build an [`McpServer`] from the relevant `AdminState` fields. Used by
    /// the HTTP MCP transport (`mcp_handler::mcp_http_handler`) to hand off
    /// each request to a fresh server instance for the spawn-blocking call.
    pub(crate) fn mcp_server(&self) -> McpServer {
        McpServer {
            pool: self.pool.clone(),
            registry: Arc::clone(&self.registry),
            runner: self.hook_runner.clone(),
            config: self.config.clone(),
            config_dir: self.config_dir.clone(),
            event_transport: self.event_transport.clone(),
            invalidation_transport: Some(self.invalidation_transport.clone()),
            cache: self.cache.clone(),
            storage: Some(self.storage.clone()),
            // HTTP transport: every request gets a fresh `McpServer`,
            // so `client_name` never gets populated by `initialize`
            // (the request that initialized is a different instance).
            // Audit logs fall back to `transport_label = "http"`.
            // Per-session identity propagation needs `Mcp-Session-Id`
            // tracking — tracked separately.
            client_name: OnceLock::new(),
            transport_label: "(http)",
        }
    }
}
