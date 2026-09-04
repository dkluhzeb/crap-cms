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

/// Email delivery provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailProvider {
    /// SMTP server (the default). Disabled when `smtp_host` is empty.
    #[default]
    Smtp,
    /// POST each message to a webhook URL.
    Webhook,
    /// Log messages instead of sending (development).
    Log,
    /// Provider registered from Lua via `crap.email.register`.
    Custom,
}

/// SMTP email configuration. Empty `smtp_host` disables email (no-op sends).
#[derive(Debug, Clone, Deserialize, Serialize, crap_cms_macros::ConfigKeys)]
#[serde(default, deny_unknown_fields)]
pub struct EmailConfig {
    /// Email provider: `smtp` (default), `webhook`, `log`, or `custom`.
    pub provider: EmailProvider,
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
}

fn default_smtp_timeout() -> u64 {
    30
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            provider: EmailProvider::default(),
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CrapConfig;

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

    /// Regression: the removed `[email]` queue-* fields surface a
    /// clear "unknown field" error for operators carrying alpha.8
    /// configs forward, so they land on the CHANGELOG / migration
    /// docs that explain the move to `[jobs.queues.email]`.
    #[test]
    fn removed_email_queue_fields_rejected() {
        for (field, value) in [
            ("queue_retries", "5"),
            ("queue_name", "\"mail\""),
            ("queue_timeout", "60"),
            ("queue_concurrency", "8"),
        ] {
            let tmp = tempfile::tempdir().expect("tempdir");
            std::fs::write(
                tmp.path().join("crap.toml"),
                format!("[email]\n{field} = {value}\n"),
            )
            .unwrap();
            let err = CrapConfig::load(tmp.path()).unwrap_err();
            let chain = format!("{err:#}");
            assert!(
                chain.contains(field),
                "expected error to mention removed `{field}`; got chain: {chain}"
            );
        }
    }
}
