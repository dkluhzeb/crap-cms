//! SMTP email provider via `lettre`.

use std::time::Duration;

use anyhow::{Context as _, Result};
use lettre::{
    Message, SmtpTransport, Transport,
    message::{Mailbox, MultiPart, SinglePart, header::ContentType},
    transport::smtp::authentication::Credentials,
};
use tracing::info;

use crate::config::{EmailConfig, SmtpTls};

use super::EmailProvider;

/// SMTP email provider.
pub struct SmtpEmailProvider {
    config: EmailConfig,
}

impl SmtpEmailProvider {
    pub fn new(config: &EmailConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }
}

impl EmailProvider for SmtpEmailProvider {
    fn send(&self, to: &str, subject: &str, html: &str, text: Option<&str>) -> Result<()> {
        send_email_smtp(&self.config, to, subject, html, text)
    }

    fn kind(&self) -> &'static str {
        "smtp"
    }
}

/// Build the MIME message (multipart if text body provided, HTML-only otherwise).
fn build_message(
    config: &EmailConfig,
    to: &str,
    subject: &str,
    html: &str,
    text: Option<&str>,
) -> Result<Message> {
    let from: Mailbox = format!("{} <{}>", config.from_name, config.from_address)
        .parse()
        .with_context(|| {
            format!(
                "Invalid from address: {} <{}>",
                config.from_name, config.from_address
            )
        })?;

    let to_mailbox: Mailbox = to
        .parse()
        .with_context(|| format!("Invalid recipient address: {}", to))?;

    if let Some(plain) = text {
        Message::builder()
            .from(from)
            .to(to_mailbox)
            .subject(subject)
            .multipart(
                MultiPart::alternative()
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(plain.to_string()),
                    )
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(html.to_string()),
                    ),
            )
            .context("Failed to build email message")
    } else {
        Message::builder()
            .from(from)
            .to(to_mailbox)
            .subject(subject)
            .header(ContentType::TEXT_HTML)
            .body(html.to_string())
            .context("Failed to build email message")
    }
}

/// Build the SMTP transport with credentials and TLS mode.
fn build_transport(config: &EmailConfig) -> Result<SmtpTransport> {
    let creds = Credentials::new(
        config.smtp_user.clone(),
        config.smtp_pass.as_ref().to_string(),
    );
    let timeout = Duration::from_secs(config.smtp_timeout);

    let transport = match config.smtp_tls {
        SmtpTls::Starttls => SmtpTransport::starttls_relay(&config.smtp_host)
            .with_context(|| {
                format!(
                    "Failed to create SMTP STARTTLS transport for {}",
                    config.smtp_host
                )
            })?
            .port(config.smtp_port)
            .credentials(creds)
            .timeout(Some(timeout))
            .build(),
        SmtpTls::Tls => SmtpTransport::relay(&config.smtp_host)
            .with_context(|| {
                format!(
                    "Failed to create SMTP TLS transport for {}",
                    config.smtp_host
                )
            })?
            .port(config.smtp_port)
            .credentials(creds)
            .timeout(Some(timeout))
            .build(),
        SmtpTls::None => SmtpTransport::builder_dangerous(&config.smtp_host)
            .port(config.smtp_port)
            .credentials(creds)
            .timeout(Some(timeout))
            .build(),
    };

    Ok(transport)
}

/// Send an email via SMTP transport.
#[cfg(not(tarpaulin_include))]
pub(super) fn send_email_smtp(
    config: &EmailConfig,
    to: &str,
    subject: &str,
    html: &str,
    text: Option<&str>,
) -> Result<()> {
    let message = build_message(config, to, subject, html, text)?;
    let transport = build_transport(config)?;

    transport
        .send(&message)
        .with_context(|| format!("Failed to send email to {}", to))?;

    info!("Email sent to {} (subject: {})", to, subject);

    Ok(())
}
