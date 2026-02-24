//! Email sending (SMTP via lettre) and email template rendering (Handlebars).

use anyhow::{Context, Result};
use handlebars::Handlebars;
use include_dir::{include_dir, Dir};
use std::path::Path;

use crate::config::EmailConfig;

static EMAIL_TEMPLATES_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates/email");

/// Renders email templates using Handlebars with overlay support.
/// Separate from admin templates — has its own Handlebars instance.
pub struct EmailRenderer {
    hbs: Handlebars<'static>,
}

impl EmailRenderer {
    /// Create a new EmailRenderer, loading compiled-in defaults then overlaying
    /// config dir templates from `<config_dir>/templates/email/`.
    pub fn new(config_dir: &Path) -> Result<Self> {
        let mut hbs = Handlebars::new();
        hbs.set_strict_mode(false);

        // Register compiled-in email templates
        for file in EMAIL_TEMPLATES_DIR.files() {
            let path = file.path();
            if path.extension().is_some_and(|ext| ext == "hbs") {
                let name = path.with_extension("").to_string_lossy().to_string();
                let content = std::str::from_utf8(file.contents())
                    .with_context(|| format!("Invalid UTF-8 in email template: {}", name))?;
                hbs.register_template_string(&name, content)
                    .with_context(|| format!("Failed to register email template: {}", name))?;
            }
        }

        // Overlay with config dir email templates
        let overlay_dir = config_dir.join("templates/email");
        if overlay_dir.exists() {
            for entry in std::fs::read_dir(&overlay_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "hbs") {
                    let name = path.file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let content = std::fs::read_to_string(&path)?;
                    tracing::debug!("Overlay email template: {}", name);
                    hbs.register_template_string(&name, &content)?;
                }
            }
        }

        Ok(Self { hbs })
    }

    /// Render an email template by name with the given data.
    pub fn render(&self, template: &str, data: &serde_json::Value) -> Result<String> {
        self.hbs.render(template, data)
            .with_context(|| format!("Failed to render email template '{}'", template))
    }
}

/// Send an email via SMTP. Blocking — call from `spawn_blocking` context.
///
/// If `smtp_host` is empty, logs a warning and returns Ok (no-op).
pub fn send_email(
    config: &EmailConfig,
    to: &str,
    subject: &str,
    html: &str,
    text: Option<&str>,
) -> Result<()> {
    if config.smtp_host.is_empty() {
        tracing::warn!("Email not configured (smtp_host empty), skipping send to {}", to);
        return Ok(());
    }

    use lettre::{
        Message, SmtpTransport, Transport,
        message::{Mailbox, header::ContentType, MultiPart, SinglePart},
        transport::smtp::authentication::Credentials,
    };

    let from: Mailbox = format!("{} <{}>", config.from_name, config.from_address)
        .parse()
        .with_context(|| format!("Invalid from address: {} <{}>", config.from_name, config.from_address))?;

    let to_mailbox: Mailbox = to.parse()
        .with_context(|| format!("Invalid recipient address: {}", to))?;

    let message = if let Some(plain) = text {
        Message::builder()
            .from(from)
            .to(to_mailbox)
            .subject(subject)
            .multipart(
                MultiPart::alternative()
                    .singlepart(SinglePart::builder()
                        .header(ContentType::TEXT_PLAIN)
                        .body(plain.to_string()))
                    .singlepart(SinglePart::builder()
                        .header(ContentType::TEXT_HTML)
                        .body(html.to_string()))
            )
            .context("Failed to build email message")?
    } else {
        Message::builder()
            .from(from)
            .to(to_mailbox)
            .subject(subject)
            .header(ContentType::TEXT_HTML)
            .body(html.to_string())
            .context("Failed to build email message")?
    };

    let creds = Credentials::new(config.smtp_user.clone(), config.smtp_pass.clone());

    let transport = SmtpTransport::starttls_relay(&config.smtp_host)
        .with_context(|| format!("Failed to create SMTP transport for {}", config.smtp_host))?
        .port(config.smtp_port)
        .credentials(creds)
        .build();

    transport.send(&message)
        .with_context(|| format!("Failed to send email to {}", to))?;

    tracing::info!("Email sent to {} (subject: {})", to, subject);
    Ok(())
}

/// Check if email is configured (smtp_host is non-empty).
pub fn is_configured(config: &EmailConfig) -> bool {
    !config.smtp_host.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::EmailConfig;

    #[test]
    fn renderer_new_loads_compiled_templates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        // Should be able to render the compiled-in password_reset template
        let result = renderer.render("password_reset", &serde_json::json!({
            "reset_url": "http://example.com/reset?token=abc",
            "app_name": "Test",
        }));
        assert!(result.is_ok(), "Should render password_reset template: {:?}", result.err());
        let html = result.unwrap();
        assert!(html.contains("reset") || html.contains("password"), "Rendered template should contain reset-related content");
    }

    #[test]
    fn renderer_overlay_replaces_template() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let email_dir = tmp.path().join("templates/email");
        std::fs::create_dir_all(&email_dir).unwrap();
        // Use triple-brace {{{reset_url}}} to avoid HTML escaping
        std::fs::write(
            email_dir.join("password_reset.hbs"),
            "<p>Custom reset: {{{reset_url}}}</p>",
        ).unwrap();

        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        let html = renderer.render("password_reset", &serde_json::json!({
            "reset_url": "http://example.com/reset",
        })).expect("render");
        assert!(html.contains("Custom reset:"), "Should use the overlaid template");
        assert!(html.contains("http://example.com/reset"));
    }

    #[test]
    fn is_configured_empty_host_false() {
        let config = EmailConfig::default();
        assert!(!is_configured(&config));
    }

    #[test]
    fn is_configured_with_host_true() {
        let mut config = EmailConfig::default();
        config.smtp_host = "smtp.example.com".to_string();
        assert!(is_configured(&config));
    }
}
