//! Admin UI: Axum server, Handlebars templates, and HTMX-powered handlers.
//!
//! ## Architecture
//!
//! - **`server`** — Axum router setup, request middleware, server lifecycle.
//!   Routes are wired up in [`server::build_router`].
//! - **`server_builder`** — typed builder for the parameter bundle that
//!   [`server::start`] consumes from `commands/serve`.
//! - **`auth_middleware`** — JWT session validation (cookie-based), custom
//!   auth-strategy fallback, and admin-access gate (Lua hook). Wraps
//!   `next.run(request)` for the rest of the admin routes.
//! - **`handlers`** — one route handler per file, organized by feature
//!   (`auth/`, `collections/`, `globals/`, `events/`, `uploads/`, …). Shared
//!   helpers under `handlers::shared` (response builders, paths, paginations,
//!   access checks) and `handlers::field_context` (template context for the
//!   collection edit form).
//! - **`templates`** — Handlebars registry + custom helpers (`{{t key}}`,
//!   `{{render_field}}`, etc.). Templates are looked up from the config dir
//!   first, embedded defaults second.
//! - **`context`** — typed page-context structs that page handlers populate
//!   and serialize to JSON for Handlebars rendering.
//! - **`custom_pages`** — discovered `<config_dir>/templates/pages/<slug>.hbs`
//!   files, surfaced as `/admin/p/<slug>` routes.
//! - **`translations`** — i18n string table (English + German baseline).
//! - **`mcp_handler`** — MCP HTTP transport bolted onto the admin port for
//!   convenience; reads its own API key from `mcp.api_key`.
//!
//! ## Cross-module conventions
//!
//! - **Cookies**: names are `pub(in crate::admin)` constants in
//!   [`handlers::auth::session`] (`SESSION_COOKIE`, `MFA_PENDING_COOKIE`,
//!   `CSRF_COOKIE`, `EDITOR_LOCALE_COOKIE`, `SESSION_EXP_COOKIE`). Never
//!   spell the literal string at a call site.
//! - **URLs**: the `/admin/...` paths come from typed builders in
//!   [`handlers::shared::paths`] (`paths::login()`, `paths::collection(slug)`,
//!   …). Constants for static paths (`paths::LOGIN`,
//!   `paths::COLLECTIONS_ROOT`); functions for slug/id-bearing paths.
//! - **Errors**: handlers convert `Result<_, ServiceError>` via
//!   [`handlers::shared::service_error_to_admin_response`] (HTML 403 / 500
//!   pages) and `JoinError` via
//!   [`handlers::shared::task_join_error_response`].
//! - **Spawn-blocking**: per CLAUDE.md, the closure body is a single named
//!   fn call. Captures get bundled into a typed `*BlockingInput` struct.
//!
//! ## State
//!
//! [`AdminState`] is the application-wide bundle handed to every handler via
//! `State<AdminState>`. It is `Clone` (cheap — Arcs and small handles). Helper
//! methods like [`AdminState::email_context`] and [`AdminState::mcp_server`]
//! bundle subsets of fields for downstream consumers without exposing the
//! struct's internals.

mod auth_middleware;
pub(crate) mod context;
mod csp_nonce;
pub mod custom_pages;
pub mod handlers;
mod mcp_handler;
pub mod server;
pub(crate) mod server_builder;
pub mod templates;
pub mod translations;

pub use csp_nonce::{CSP_NONCE, CspNonce, current_nonce_or_empty};
pub use translations::Translations;

// Workspace-split prep: surface cross-crate helpers at the admin root so
// callers in `api::upload` and `service::upload` import via
// `crate::admin::Foo` instead of reaching into `admin::handlers::Foo`.
// Stays `pub(crate)` because both consumers live in this crate; a future
// workspace split would promote to `pub`.
pub(crate) use handlers::{extract_join_data_from_form, parse_multipart_form};

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
    mcp::McpServer,
    service::EmailContext,
};

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
    pub fn render(&self, template: &str, data: &Value) -> Result<String, String> {
        self.handlebars
            .render(template, data)
            .map_err(|e| format!("Template error: {}", e))
    }

    /// Bundle the email config + renderer + server config into an
    /// `EmailContext` for verification email flows.
    pub(crate) fn email_context(&self) -> EmailContext {
        EmailContext {
            email_config: self.config.email.clone(),
            email_renderer: self.email_renderer.clone(),
            server_config: self.config.server.clone(),
        }
    }

    /// Build an [`McpServer`] from the relevant `AdminState` fields. Used by
    /// the HTTP MCP transport (`mcp_handler::mcp_http_handler`) to hand off
    /// each request to a fresh server instance for the spawn-blocking call.
    pub(crate) fn mcp_server(&self) -> McpServer {
        McpServer {
            pool: self.pool.clone(),
            registry: self.registry.clone(),
            runner: self.hook_runner.clone(),
            config: self.config.clone(),
            config_dir: self.config_dir.clone(),
            event_transport: self.event_transport.clone(),
            invalidation_transport: Some(self.invalidation_transport.clone()),
            cache: self.cache.clone(),
        }
    }
}
