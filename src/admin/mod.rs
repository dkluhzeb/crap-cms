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
use crate::core::email::EmailRenderer;
use crate::core::event::EventBus;
use crate::core::rate_limit::LoginRateLimiter;
use crate::core::Registry;
use crate::db::DbPool;
use crate::hooks::lifecycle::HookRunner;

use self::translations::Translations;

/// Shared state for all admin handlers.
#[derive(Clone)]
#[allow(dead_code)]
pub struct AdminState {
    pub config: CrapConfig,
    pub config_dir: PathBuf,
    pub pool: DbPool,
    pub registry: Arc<Registry>,
    pub handlebars: Arc<Handlebars<'static>>,
    pub hook_runner: HookRunner,
    pub jwt_secret: String,
    pub email_renderer: Arc<EmailRenderer>,
    pub event_bus: Option<EventBus>,
    pub login_limiter: Arc<LoginRateLimiter>,
    pub forgot_password_limiter: Arc<LoginRateLimiter>,
    pub has_auth: bool,
    pub translations: Arc<Translations>,
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
