//! `AdminState` — the application-wide bundle handed to every admin
//! handler via `axum::extract::State<AdminState>`. Plus the helper
//! methods that derive sub-bundles (`mcp_server`) from the shared `infra`.

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock, atomic::AtomicUsize},
};

use handlebars::Handlebars;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    config::CrapConfig,
    core::{JwtSecret, SharedEmailProvider, SharedPasswordProvider, rate_limit::LoginRateLimiter},
    mcp::McpServer,
    service::AppInfra,
};

use super::{Translations, custom_pages};

/// Shared state for all admin handlers.
#[derive(Clone)]
pub struct AdminState {
    /// Process-stable infrastructure bundle — pool, registry, hook runner,
    /// caches, transports, storage, token provider, email context. Handlers read
    /// individual deps as `state.infra.pool`, `state.infra.registry`, … and write
    /// paths thread it into a `ServiceContext` via `.infra(&state.infra)`.
    pub infra: Arc<AppInfra>,
    /// The global configuration for the CMS.
    pub config: CrapConfig,
    /// The directory where the configuration is located.
    pub config_dir: PathBuf,
    /// The Handlebars template engine instance.
    pub handlebars: Arc<Handlebars<'static>>,
    /// The secret key used for signing and verifying JWTs.
    pub jwt_secret: JwtSecret,
    /// The email provider for sending emails.
    pub email_provider: SharedEmailProvider,
    /// The rate limiter for login attempts (per-email).
    pub login_limiter: Arc<LoginRateLimiter>,
    /// The rate limiter for login attempts (per-IP).
    pub ip_login_limiter: Arc<LoginRateLimiter>,
    /// The rate limiter for password reset requests (per-email).
    pub forgot_password_limiter: Arc<LoginRateLimiter>,
    /// The rate limiter for password reset requests (per-IP).
    pub ip_forgot_password_limiter: Arc<LoginRateLimiter>,
    /// The rate limiter for MFA code verification (per-user). Independent of the
    /// login limiter so a successful password (which clears the login limiter
    /// before issuing the MFA challenge) can't reset the MFA brute-force budget.
    pub mfa_limiter: Arc<LoginRateLimiter>,
    /// The rate limiter for MFA code verification (per-IP).
    pub ip_mfa_limiter: Arc<LoginRateLimiter>,
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
    /// The password provider for hashing and verification.
    pub password_provider: SharedPasswordProvider,
    /// Per-subscriber SSE send timeout in milliseconds.
    pub subscriber_send_timeout_ms: u64,
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

    /// Build an [`McpServer`] for the HTTP MCP transport, sharing the admin's
    /// process-stable infra bundle (a fresh `McpServer` wrapper per request, the
    /// same `Arc<AppInfra>`).
    pub(crate) fn mcp_server(&self) -> McpServer {
        McpServer {
            infra: Arc::clone(&self.infra),
            config: self.config.clone(),
            config_dir: self.config_dir.clone(),
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
