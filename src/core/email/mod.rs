//! Email sending abstraction and template rendering.
//!
//! The `EmailProvider` trait allows pluggable email backends:
//! `smtp` (default), `webhook` (HTTP API), `log` (dev mode), `custom` (Lua).

mod custom;
mod factory;
mod log;
pub mod queue;
mod renderer;
mod smtp;
mod validation;
mod webhook;

use std::sync::Arc;

use anyhow::Result;

pub use custom::CustomEmailProvider;
pub use factory::{create_email_provider, is_configured};
pub use queue::{EmailJobData, SYSTEM_EMAIL_JOB, queue_email};
pub use renderer::EmailRenderer;
pub use validation::validate_no_crlf;

/// Thread-safe shared reference to an email provider.
pub type SharedEmailProvider = Arc<dyn EmailProvider>;

/// Object-safe email provider trait.
pub trait EmailProvider: Send + Sync {
    /// Send an email. Blocking — call from `spawn_blocking` context.
    fn send(&self, to: &str, subject: &str, html: &str, text: Option<&str>) -> Result<()>;

    /// Return the backend identifier (`"smtp"`, `"webhook"`, `"log"`, `"custom"`).
    fn kind(&self) -> &'static str;
}
