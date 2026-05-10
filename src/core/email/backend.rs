//! `EmailProvider` trait + `SharedEmailProvider` type alias — the
//! abstraction every email backend in this module satisfies.

use std::sync::Arc;

use anyhow::Result;

/// Thread-safe shared reference to an email provider.
pub type SharedEmailProvider = Arc<dyn EmailProvider>;

/// Object-safe email provider trait.
pub trait EmailProvider: Send + Sync {
    /// Send an email. Blocking — call from `spawn_blocking` context.
    fn send(&self, to: &str, subject: &str, html: &str, text: Option<&str>) -> Result<()>;

    /// Return the backend identifier (`"smtp"`, `"webhook"`, `"log"`, `"custom"`).
    fn kind(&self) -> &'static str;
}
