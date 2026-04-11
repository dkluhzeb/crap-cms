//! Email template rendering via Handlebars with overlay support.

use std::{fs, path::Path, str};

use anyhow::{Context as _, Result};
use handlebars::Handlebars;
use include_dir::{Dir, include_dir};
use serde_json::Value;
use tracing::debug;

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
                let content = str::from_utf8(file.contents())
                    .with_context(|| format!("Invalid UTF-8 in email template: {}", name))?;
                hbs.register_template_string(&name, content)
                    .with_context(|| format!("Failed to register email template: {}", name))?;
            }
        }

        // Overlay with config dir email templates
        let overlay_dir = config_dir.join("templates/email");

        if overlay_dir.exists() {
            for entry in fs::read_dir(&overlay_dir)? {
                let entry = entry?;
                let path = entry.path();

                if path.extension().is_some_and(|ext| ext == "hbs") {
                    let name = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let content = fs::read_to_string(&path)?;

                    debug!("Overlay email template: {}", name);

                    hbs.register_template_string(&name, &content)?;
                }
            }
        }

        Ok(Self { hbs })
    }

    /// Render an email template by name with the given data.
    pub fn render(&self, template: &str, data: &Value) -> Result<String> {
        self.hbs
            .render(template, data)
            .with_context(|| format!("Failed to render email template '{}'", template))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn renderer_new_loads_compiled_templates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        let result = renderer.render(
            "password_reset",
            &json!({
                "reset_url": "http://example.com/reset?token=abc",
                "app_name": "Test",
            }),
        );
        assert!(result.is_ok());
        let html = result.unwrap();
        assert!(html.contains("reset") || html.contains("password"));
    }

    #[test]
    fn renderer_overlay_replaces_template() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let email_dir = tmp.path().join("templates/email");
        std::fs::create_dir_all(&email_dir).unwrap();
        std::fs::write(
            email_dir.join("password_reset.hbs"),
            "<p>Custom reset: {{{reset_url}}}</p>",
        )
        .unwrap();

        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        let html = renderer
            .render(
                "password_reset",
                &json!({"reset_url": "http://example.com/reset"}),
            )
            .expect("render");
        assert!(html.contains("Custom reset:"));
    }

    #[test]
    fn renderer_render_missing_template() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        let result = renderer.render("nonexistent_template", &json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn renderer_no_overlay_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        let result = renderer.render(
            "password_reset",
            &json!({"reset_url": "http://example.com/reset"}),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn renderer_render_empty_data() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        let result = renderer.render("password_reset", &json!({}));
        assert!(result.is_ok());
    }
}
