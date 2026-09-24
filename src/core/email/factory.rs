//! Email provider factory — creates the appropriate backend from config.

use std::{
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::Result;
use tracing::{info, warn};

use crate::config::{EmailConfig, EmailProvider, SmtpTls};
use crate::core::lua_lease::LuaVmLease;

use super::{CustomEmailProvider, SharedEmailProvider, log::LogEmailProvider, smtp, webhook};

/// Whether the "SMTP host empty — using log provider" notice was already
/// logged by this process.
static LOG_PROVIDER_NOTICED: AtomicBool = AtomicBool::new(false);

/// Whether the plaintext-SMTP warning was already logged by this process.
static PLAINTEXT_SMTP_NOTICED: AtomicBool = AtomicBool::new(false);

/// `true` the first time it is called for `flag`, `false` ever after.
///
/// A provider is built once per Lua VM (every pool VM registers
/// `crap.email`) and once per server surface, but the notices describe the
/// process's configuration — one line per process, not one per VM.
fn first_notice(flag: &AtomicBool) -> bool {
    !flag.swap(true, Ordering::Relaxed)
}

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
                if first_notice(&LOG_PROVIDER_NOTICED) {
                    info!("Email SMTP host empty — using log provider");
                }

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
/// quiet. The warning fires once per process — not once per Lua VM that
/// builds a provider — and the per-email send path is intentionally left
/// silent to avoid log spam.
fn warn_on_plaintext_smtp(config: &EmailConfig) {
    if config.smtp_tls != SmtpTls::None {
        return;
    }

    if is_loopback_host(&config.smtp_host) {
        return;
    }

    if !first_notice(&PLAINTEXT_SMTP_NOTICED) {
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

    /// Regression: the provider notice was logged once per Lua VM (every pool
    /// VM builds a provider when it registers `crap.email`) — two dozen
    /// identical lines per boot. A notice flag answers "first" exactly once.
    #[test]
    fn a_notice_flag_answers_first_exactly_once() {
        let flag = AtomicBool::new(false);

        assert!(first_notice(&flag));
        assert!(!first_notice(&flag));
        assert!(!first_notice(&flag));
    }

    /// Building the provider many times — as every pool VM does — keeps
    /// working after the notice has been spent.
    #[test]
    fn repeated_builds_still_yield_the_log_provider() {
        let config = EmailConfig::default();

        for _ in 0..3 {
            let provider = create_email_provider(&config).expect("provider");
            assert_eq!(provider.kind(), "log");
        }
    }

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
