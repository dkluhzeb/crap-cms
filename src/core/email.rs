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

    send_email_smtp(config, to, subject, html, text)
}

/// Actually send the email via SMTP transport. Requires a real SMTP server —
/// cannot be unit-tested without external infrastructure.
#[cfg(not(tarpaulin_include))]
fn send_email_smtp(
    config: &EmailConfig,
    to: &str,
    subject: &str,
    html: &str,
    text: Option<&str>,
) -> Result<()> {
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

    let timeout = std::time::Duration::from_secs(config.smtp_timeout);
    let transport = SmtpTransport::starttls_relay(&config.smtp_host)
        .with_context(|| format!("Failed to create SMTP transport for {}", config.smtp_host))?
        .port(config.smtp_port)
        .credentials(creds)
        .timeout(Some(timeout))
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
        let config = EmailConfig {
            smtp_host: "smtp.example.com".to_string(),
            ..Default::default()
        };
        assert!(is_configured(&config));
    }

    #[test]
    fn send_email_noop_when_host_empty() {
        let config = EmailConfig::default();
        // With empty smtp_host, send_email should return Ok without doing anything
        let result = send_email(&config, "user@example.com", "Test", "<p>Hello</p>", None);
        assert!(result.is_ok(), "Empty smtp_host should be a no-op: {:?}", result.err());
    }

    #[test]
    fn send_email_noop_with_text_body() {
        let config = EmailConfig::default();
        let result = send_email(
            &config,
            "user@example.com",
            "Test",
            "<p>Hello</p>",
            Some("Hello plain text"),
        );
        assert!(result.is_ok(), "Empty smtp_host should be a no-op even with text body");
    }

    #[test]
    fn renderer_render_missing_template() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        let result = renderer.render("nonexistent_template", &serde_json::json!({}));
        assert!(result.is_err(), "Rendering a missing template should fail");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("nonexistent_template"),
            "Error should mention the template name: {}",
            err_msg
        );
    }

    #[test]
    fn renderer_render_verify_email_template() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        let result = renderer.render("verify_email", &serde_json::json!({
            "verify_url": "http://example.com/verify?token=xyz",
            "from_name": "Test CMS",
        }));
        assert!(result.is_ok(), "Should render verify_email template: {:?}", result.err());
        let html = result.unwrap();
        assert!(html.contains("verify") || html.contains("Verify"),
            "Rendered template should contain verify-related content");
    }

    #[test]
    fn renderer_render_with_data() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let email_dir = tmp.path().join("templates/email");
        std::fs::create_dir_all(&email_dir).unwrap();
        std::fs::write(
            email_dir.join("custom.hbs"),
            "Hello {{{name}}}, your code is {{{code}}}.",
        ).unwrap();

        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        let html = renderer.render("custom", &serde_json::json!({
            "name": "Alice",
            "code": "ABC123",
        })).expect("render");
        assert_eq!(html, "Hello Alice, your code is ABC123.");
    }

    #[test]
    fn renderer_overlay_ignores_non_hbs_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let email_dir = tmp.path().join("templates/email");
        std::fs::create_dir_all(&email_dir).unwrap();
        // Write a non-.hbs file — should be ignored
        std::fs::write(email_dir.join("notes.txt"), "This should be ignored").unwrap();
        // Write a .hbs file — should be loaded
        std::fs::write(email_dir.join("test.hbs"), "Test: {{{val}}}").unwrap();

        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        // test.hbs should be loadable
        let result = renderer.render("test", &serde_json::json!({"val": "ok"}));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Test: ok");
        // notes.txt should not be registered as a template
        let result2 = renderer.render("notes", &serde_json::json!({}));
        assert!(result2.is_err(), "non-hbs files should not be registered as templates");
    }

    #[test]
    fn renderer_no_overlay_dir() {
        // When there's no templates/email directory at all, the renderer should still work
        // (just using compiled-in templates)
        let tmp = tempfile::tempdir().expect("tempdir");
        // Don't create templates/email — it doesn't exist
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        // Should still be able to render compiled-in templates
        let result = renderer.render("password_reset", &serde_json::json!({
            "reset_url": "http://example.com/reset",
        }));
        assert!(result.is_ok());
    }

    #[test]
    fn renderer_render_empty_data() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        // Render with empty data — Handlebars in non-strict mode should still succeed,
        // just leaving placeholders empty
        let result = renderer.render("password_reset", &serde_json::json!({}));
        assert!(result.is_ok(), "Rendering with missing data should succeed in non-strict mode");
    }
}
