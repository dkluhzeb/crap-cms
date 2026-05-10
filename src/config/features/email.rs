//! SMTP / webhook email configuration.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::config::{SmtpPassword, parsing::serde_duration};

/// SMTP TLS mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum SmtpTls {
    /// Connect plain, upgrade via STARTTLS (port 587).
    #[default]
    Starttls,
    /// Implicit TLS from the start (port 465).
    Tls,
    /// No encryption (local/test servers, port 25/1025).
    None,
}

/// SMTP email configuration. Empty `smtp_host` disables email (no-op sends).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct EmailConfig {
    /// Email provider: `"smtp"` (default), `"webhook"`, `"log"`, or `"custom"`.
    #[serde(default = "default_email_provider")]
    pub provider: String,
    /// SMTP server hostname. Empty = email disabled (falls back to log provider).
    pub smtp_host: String,
    /// SMTP server port (default 587).
    pub smtp_port: u16,
    /// SMTP username for authentication.
    pub smtp_user: String,
    /// SMTP password for authentication.
    pub smtp_pass: SmtpPassword,
    /// "From" email address (default "noreply@example.com").
    pub from_address: String,
    /// "From" display name (default "Crap CMS").
    pub from_name: String,
    /// TLS mode: "starttls" (default), "tls" (implicit), "none" (plain).
    pub smtp_tls: SmtpTls,
    /// SMTP connection/send timeout in seconds (default 30).
    #[serde(default = "default_smtp_timeout", with = "serde_duration")]
    pub smtp_timeout: u64,
    /// Webhook URL for the webhook email provider.
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// Extra HTTP headers for webhook requests (e.g., Authorization).
    #[serde(default)]
    pub webhook_headers: HashMap<String, String>,
    /// Retry count for queued emails via `crap.email.queue()`. Default: 3.
    #[serde(default = "default_queue_retries")]
    pub queue_retries: u32,
    /// Job queue name for queued emails. Default: "email".
    #[serde(default = "default_queue_name")]
    pub queue_name: String,
    /// Per-attempt timeout for queued email jobs in seconds. Default: 30.
    #[serde(default = "default_queue_timeout")]
    pub queue_timeout: u64,
    /// Max concurrent queued email jobs. Default: 5.
    #[serde(default = "default_queue_concurrency")]
    pub queue_concurrency: u32,
}

fn default_queue_retries() -> u32 {
    3
}

fn default_queue_name() -> String {
    "email".to_string()
}

fn default_queue_timeout() -> u64 {
    30
}

fn default_queue_concurrency() -> u32 {
    5
}

fn default_email_provider() -> String {
    "smtp".to_string()
}

fn default_smtp_timeout() -> u64 {
    30
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            provider: default_email_provider(),
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_user: String::new(),
            smtp_pass: SmtpPassword::new(""),
            from_address: "noreply@example.com".to_string(),
            from_name: "Crap CMS".to_string(),
            smtp_tls: SmtpTls::default(),
            smtp_timeout: 30,
            webhook_url: None,
            webhook_headers: HashMap::new(),
            queue_retries: default_queue_retries(),
            queue_name: default_queue_name(),
            queue_timeout: default_queue_timeout(),
            queue_concurrency: default_queue_concurrency(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_config_defaults() {
        let email = EmailConfig::default();
        assert!(
            email.smtp_host.is_empty(),
            "smtp_host should be empty by default"
        );
        assert_eq!(email.smtp_port, 587);
        assert!(email.smtp_user.is_empty());
        assert!(email.smtp_pass.is_empty());
        assert_eq!(email.smtp_tls, SmtpTls::Starttls);
        assert_eq!(email.from_address, "noreply@example.com");
        assert_eq!(email.from_name, "Crap CMS");
    }
}
