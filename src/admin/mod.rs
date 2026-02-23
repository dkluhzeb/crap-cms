//! Admin UI: Axum server, Handlebars templates, and HTMX-powered handlers.

pub mod server;
pub mod templates;
pub mod handlers;

use std::path::PathBuf;
use std::sync::Arc;

use handlebars::Handlebars;

use crate::config::CrapConfig;
use crate::core::SharedRegistry;
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
}

impl AdminState {
    /// Render a template with the given data, returning HTML string.
    pub fn render(&self, template: &str, data: &serde_json::Value) -> Result<String, String> {
        self.handlebars.render(template, data)
            .map_err(|e| format!("Template error: {}", e))
    }

    /// Get collection info for the sidebar navigation.
    pub fn sidebar_collections(&self) -> Vec<serde_json::Value> {
        let reg = match self.registry.read() {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Registry lock poisoned: {}", e);
                return Vec::new();
            }
        };
        let mut collections: Vec<_> = reg.collections.values()
            .map(|def| {
                serde_json::json!({
                    "slug": def.slug,
                    "display_name": def.display_name(),
                })
            })
            .collect();
        collections.sort_by(|a, b| {
            a["slug"].as_str().cmp(&b["slug"].as_str())
        });
        collections
    }

    /// Get global info for the sidebar navigation.
    pub fn sidebar_globals(&self) -> Vec<serde_json::Value> {
        let reg = match self.registry.read() {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Registry lock poisoned: {}", e);
                return Vec::new();
            }
        };
        let mut globals: Vec<_> = reg.globals.values()
            .map(|def| {
                serde_json::json!({
                    "slug": def.slug,
                    "display_name": def.display_name(),
                })
            })
            .collect();
        globals.sort_by(|a, b| {
            a["slug"].as_str().cmp(&b["slug"].as_str())
        });
        globals
    }
}
