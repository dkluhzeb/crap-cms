//! Email provider factory — creates the appropriate backend from config.

use std::{net::IpAddr, sync::Arc};

use anyhow::Result;
use tracing::{info, warn};

use crate::config::{EmailConfig, EmailProvider, SmtpTls};
use crate::core::lua_lease::LuaVmLease;

use super::{CustomEmailProvider, SharedEmailProvider, log::LogEmailProvider, smtp, webhook};

/// Check if email sending is configured.
/// Returns false if SMTP host is empty and provider is smtp (the default).
#[must_use]
pub fn is_configured(config: &EmailConfig) -> bool {
    match config.provider {
        EmailProvider::Smtp => !config.smtp_host.is_empty(),
        EmailProvider::Log => false,
        // webhook and custom are always "configured"
        EmailProvider::Webhook | EmailProvider::Custom => true,
    }
}

/// Create the appropriate email provider from config.
///
/// # Errors
///
/// Returns an error if the provider name is unknown or the chosen
/// backend fails to initialize.
pub fn create_email_provider(config: &EmailConfig) -> Result<SharedEmailProvider> {
    match config.provider {
        EmailProvider::Smtp => {
            if config.smtp_host.is_empty() {
                info!("Email SMTP host empty — using log provider");

                Ok(Arc::new(LogEmailProvider))
            } else {
                warn_on_plaintext_smtp(config);

                Ok(Arc::new(smtp::SmtpEmailProvider::new(config)))
            }
        }
        EmailProvider::Webhook => Ok(Arc::new(webhook::WebhookEmailProvider::new(config)?)),
        EmailProvider::Log => Ok(Arc::new(LogEmailProvider)),
        EmailProvider::Custom => {
            // No lease available here (config-only call site). The
            // pool/local-backed custom provider is built via
            // `create_email_provider_with_lease`; this placeholder only
            // fires if a caller forgot to use that path.
            info!("Custom email provider selected without a Lua lease — using log placeholder");
            Ok(Arc::new(LogEmailProvider))
        }
    }
}

/// Create an email provider, backing a `custom` provider with `lease`.
///
/// Use this at call sites that have a Lua VM lease (a hook-runner pool
/// lease, or a per-VM local lease) so `[email] provider = "custom"`
/// resolves to a working [`CustomEmailProvider`] instead of the log
/// placeholder. Non-custom providers ignore the lease.
///
/// # Errors
///
/// Returns an error if the underlying backend fails to initialize.
pub fn create_email_provider_with_lease(
    config: &EmailConfig,
    lease: Arc<dyn LuaVmLease>,
) -> Result<SharedEmailProvider> {
    if matches!(config.provider, EmailProvider::Custom) {
        return Ok(Arc::new(CustomEmailProvider::new(lease)));
    }
    create_email_provider(config)
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

    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
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
