//! Email provider factory — creates the appropriate backend from config.

use std::{net::IpAddr, sync::Arc};

use anyhow::{Result, bail};
use tracing::{info, warn};

use crate::config::{EmailConfig, SmtpTls};

use super::{SharedEmailProvider, log::LogEmailProvider, smtp, webhook};

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

                Ok(Arc::new(LogEmailProvider))
            } else {
                warn_on_plaintext_smtp(config);

                Ok(Arc::new(smtp::SmtpEmailProvider::new(config)))
            }
        }
        "webhook" => Ok(Arc::new(webhook::WebhookEmailProvider::new(config)?)),
        "log" => Ok(Arc::new(LogEmailProvider)),
        "custom" => {
            // Custom provider is initialized via crap.email.register() in Lua init.
            // At config load time, use log provider as placeholder — the Lua VM
            // will replace it when init.lua runs.
            info!("Custom email provider selected — waiting for Lua init");
            Ok(Arc::new(LogEmailProvider))
        }
        other => bail!("Unknown email provider: '{}'", other),
    }
}

/// Emit a startup warning when plaintext SMTP (`smtp_tls = none`) is paired
/// with a non-loopback host. Local dev SMTP (mailhog, mailpit, etc.) stays
/// quiet. The warning fires once at startup — the per-email send path is
/// intentionally left silent to avoid log spam.
fn warn_on_plaintext_smtp(config: &EmailConfig) {
    if config.smtp_tls != SmtpTls::None {
        return;
    }

    if is_loopback_host(&config.smtp_host) {
        return;
    }

    warn!(
        host = %config.smtp_host,
        port = config.smtp_port,
        "SMTP is configured with smtp_tls = \"none\" for a non-loopback host — \
         credentials and email contents travel unencrypted. Switch smtp_tls to \
         \"starttls\" or \"tls\" unless you fully control the network path."
    );
}

/// Return `true` if the hostname is a loopback target we should treat as
/// local dev: the literal "localhost", an IPv4 in 127.0.0.0/8, or `::1`.
fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    host.parse::<IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_matches_localhost_literal() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("LOCALHOST"));
    }

    #[test]
    fn loopback_matches_ipv4_127() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("127.1.2.3"));
    }

    #[test]
    fn loopback_matches_ipv6_one() {
        assert!(is_loopback_host("::1"));
    }

    #[test]
    fn loopback_rejects_non_loopback() {
        assert!(!is_loopback_host("mail.example.com"));
        assert!(!is_loopback_host("10.0.0.1"));
        assert!(!is_loopback_host("2001:db8::1"));
        assert!(!is_loopback_host(""));
    }
}
