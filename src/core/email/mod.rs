//! Email sending abstraction and template rendering.
//!
//! The `EmailProvider` trait allows pluggable email backends:
//! `smtp` (default), `webhook` (HTTP API), `log` (dev mode), `custom` (Lua).

mod custom;
mod log;
pub mod queue;
mod renderer;
mod smtp;
mod webhook;

pub use custom::CustomEmailProvider;
pub use queue::{EmailJobData, SYSTEM_EMAIL_JOB, queue_email};

use std::sync::Arc;

use anyhow::{Result, bail};
use tracing::info;

use crate::config::EmailConfig;

pub use renderer::EmailRenderer;

/// Thread-safe shared reference to an email provider.
pub type SharedEmailProvider = Arc<dyn EmailProvider>;

/// Object-safe email provider trait.
pub trait EmailProvider: Send + Sync {
    /// Send an email. Blocking — call from `spawn_blocking` context.
    fn send(&self, to: &str, subject: &str, html: &str, text: Option<&str>) -> Result<()>;

    /// Return the backend identifier (`"smtp"`, `"webhook"`, `"log"`, `"custom"`).
    fn kind(&self) -> &'static str;
}

/// Check if email sending is configured.
/// Returns false if SMTP host is empty and provider is smtp (the default).
pub fn is_configured(config: &EmailConfig) -> bool {
    match config.provider.as_str() {
        "smtp" | "" => !config.smtp_host.is_empty(),
        "log" => false,
        _ => true, // webhook, custom are always "configured"
    }
}

/// Create the appropriate email provider from config.
pub fn create_email_provider(config: &EmailConfig) -> Result<SharedEmailProvider> {
    match config.provider.as_str() {
        "smtp" | "" => {
            if config.smtp_host.is_empty() {
                info!("Email SMTP host empty — using log provider");

                Ok(Arc::new(log::LogEmailProvider))
            } else {
                Ok(Arc::new(smtp::SmtpEmailProvider::new(config)))
            }
        }
        "webhook" => Ok(Arc::new(webhook::WebhookEmailProvider::new(config)?)),
        "log" => Ok(Arc::new(log::LogEmailProvider)),
        "custom" => {
            // Custom provider is initialized via crap.email.register() in Lua init.
            // At config load time, use log provider as placeholder — the Lua VM
            // will replace it when init.lua runs.
            info!("Custom email provider selected — waiting for Lua init");
            Ok(Arc::new(log::LogEmailProvider))
        }
        other => bail!("Unknown email provider: '{}'", other),
    }
}
