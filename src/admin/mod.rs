//! Admin UI: Axum server, Handlebars templates, and HTMX-powered handlers.

pub mod context;
pub mod server;
pub mod templates;
pub mod translations;
pub mod handlers;

use std::path::PathBuf;
use std::sync::Arc;

use handlebars::Handlebars;

use crate::config::CrapConfig;
use crate::core::SharedRegistry;
use crate::core::email::EmailRenderer;
use crate::core::event::EventBus;
use crate::db::DbPool;
use crate::hooks::lifecycle::HookRunner;

/// Shared state for all admin handlers.
#[derive(Clone)]
#[allow(dead_code)]
pub struct AdminState {
    pub config: CrapConfig,
    pub config_dir: PathBuf,
    pub pool: DbPool,
    pub registry: SharedRegistry,
    pub handlebars: Arc<Handlebars<'static>>,
    pub hook_runner: HookRunner,
    pub jwt_secret: String,
    pub email_renderer: Arc<EmailRenderer>,
    pub event_bus: Option<EventBus>,
}

impl AdminState {
    /// Render a template with the given data, returning HTML string.
    pub fn render(&self, template: &str, data: &serde_json::Value) -> Result<String, String> {
        self.handlebars.render(template, data)
            .map_err(|e| format!("Template error: {}", e))
    }

}
