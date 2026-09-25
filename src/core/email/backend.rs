//! `EmailProvider` trait + `SharedEmailProvider` type alias — the
//! abstraction every email backend in this module satisfies.

use std::sync::Arc;

use anyhow::Result;

use super::EmailJobData;

/// Thread-safe shared reference to an email provider.
pub type SharedEmailProvider = Arc<dyn EmailProvider>;

/// Object-safe email provider trait.
pub trait EmailProvider: Send + Sync {
    /// Send an email. Blocking — call from `spawn_blocking` context.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying transport fails (SMTP error,
    /// webhook HTTP failure, …).
    fn send(&self, to: &str, subject: &str, html: &str, text: Option<&str>) -> Result<()>;

    /// Deliver a queued email (the `_system_email` job) within the email
    /// queue's `timeout_secs` — the scheduler runs it on a blocking thread
    /// it cannot cancel.
    ///
    /// The default is [`send`](Self::send): the SMTP and webhook transports
    /// are bounded by their own timeouts. A provider that runs user code
    /// (the custom Lua provider) overrides this to stop that code at the
    /// deadline.
    ///
    /// # Errors
    ///
    /// As [`send`](Self::send), plus the deadline's error once it passed.
    fn send_queued(&self, email: &EmailJobData, _timeout_secs: u64) -> Result<()> {
        self.send(
            &email.to,
            &email.subject,
            &email.html,
            email.text.as_deref(),
        )
    }

    /// Return the backend identifier (`"smtp"`, `"webhook"`, `"log"`, `"custom"`).
    fn kind(&self) -> &'static str;
}
