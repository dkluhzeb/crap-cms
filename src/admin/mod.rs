//! Admin UI: Axum server, Handlebars templates, and HTMX-powered handlers.

pub mod context;
pub mod handlers;
pub mod server;
pub mod templates;
pub mod translations;

use std::path::PathBuf;
use std::sync::Arc;

use handlebars::Handlebars;

use tokio_util::sync::CancellationToken;

use crate::config::CrapConfig;
use crate::core::Registry;
use crate::core::email::EmailRenderer;
use crate::core::event::EventBus;
use crate::core::rate_limit::LoginRateLimiter;
use crate::db::DbPool;
use crate::hooks::lifecycle::HookRunner;

use self::translations::Translations;

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
    pub jwt_secret: String,
    /// The renderer for email notifications.
    pub email_renderer: Arc<EmailRenderer>,
    /// The event bus for asynchronous event handling, if enabled.
    pub event_bus: Option<EventBus>,
    /// The rate limiter for login attempts.
    pub login_limiter: Arc<LoginRateLimiter>,
    /// The rate limiter for password reset requests.
    pub forgot_password_limiter: Arc<LoginRateLimiter>,
    /// Whether authentication is enabled for the admin UI.
    pub has_auth: bool,
    /// The translations for the admin UI.
    pub translations: Arc<Translations>,
    /// Token used to signal shutdown to the admin server.
    pub shutdown: CancellationToken,
}

impl AdminState {
    /// Render a template with the given data, returning HTML string.
    pub fn render(&self, template: &str, data: &serde_json::Value) -> Result<String, String> {
        self.handlebars
            .render(template, data)
            .map_err(|e| format!("Template error: {}", e))
    }
}
