//! Email template rendering via Handlebars with overlay support.

use std::{fs, path::Path, str};

use anyhow::{Context as _, Result};
use handlebars::Handlebars;
use include_dir::{Dir, include_dir};
use serde::Serialize;
use tracing::debug;

use crate::{
    admin::Translations,
    core::email::{SystemEmail, validate_no_crlf},
};

static EMAIL_TEMPLATES_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates/email");

/// Renders email templates using Handlebars with overlay support, and
/// resolves the system emails' subject lines through the admin translations.
/// Separate from admin templates — has its own Handlebars instance.
pub struct EmailRenderer {
    hbs: Handlebars<'static>,
    translations: Translations,
}

impl EmailRenderer {
    /// Create a new `EmailRenderer`, loading compiled-in defaults then overlaying
    /// config dir templates from `<config_dir>/templates/email/`, and the
    /// admin translations (with `<config_dir>/translations/` overrides) the
    /// subject lines resolve through.
    ///
    /// # Errors
    ///
    /// Returns an error if any compiled-in or overlay template fails to load or
    /// register, or a subject translation spans more than one line.
    pub fn new(config_dir: &Path) -> Result<Self> {
        let mut hbs = Handlebars::new();

        // Strict: an email context is small and fully known, so an unknown
        // variable is a typo in an overlay template, never missing data. Lax
        // mode rendered it as the empty string and sent the mail anyway — a
        // reset mail with no link, after the token was already spent. Failing
        // the render reports the send as failed instead.
        hbs.set_strict_mode(true);

        // Register compiled-in email templates
        for file in EMAIL_TEMPLATES_DIR.files() {
            let path = file.path();

            if path.extension().is_some_and(|ext| ext == "hbs") {
                let name = path.with_extension("").to_string_lossy().to_string();
                let content = str::from_utf8(file.contents())
                    .with_context(|| format!("Invalid UTF-8 in email template: {name}"))?;
                hbs.register_template_string(&name, content)
                    .with_context(|| format!("Failed to register email template: {name}"))?;
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

        let translations = Translations::load(config_dir);
        check_subjects(&translations)?;

        Ok(Self { hbs, translations })
    }

    /// The subject line of `email` in the UI `locale` — the translation of its
    /// subject key, falling back to English.
    #[must_use]
    pub fn subject(&self, email: SystemEmail, locale: &str) -> String {
        self.translations
            .get(locale, email.subject_key())
            .to_string()
    }

    /// Render an email template by name with the given typed context.
    ///
    /// # Errors
    ///
    /// Returns an error if the template name is unknown or rendering fails.
    pub fn render<T: Serialize>(&self, template: &str, data: &T) -> Result<String> {
        self.hbs
            .render(template, data)
            .with_context(|| format!("Failed to render email template '{template}'"))
    }
}

/// Every system email subject is a single header line in every UI locale. A
/// translation override spanning lines would otherwise fail each send of
/// that email; it fails the start instead.
fn check_subjects(translations: &Translations) -> Result<()> {
    for locale in translations.available_locales() {
        for email in SystemEmail::ALL {
            let key = email.subject_key();

            validate_no_crlf("subject", translations.get(locale, key))
                .with_context(|| format!("Translation '{key}' ({locale}) must be a single line"))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::core::email::{PasswordResetEmailContext, VerifyEmailContext};

    #[test]
    fn subjects_follow_the_ui_locale() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");

        assert_eq!(
            renderer.subject(SystemEmail::PasswordReset, "en"),
            "Reset your password"
        );
        assert_eq!(
            renderer.subject(SystemEmail::PasswordReset, "de"),
            "Passwort zurücksetzen"
        );
        assert_eq!(
            renderer.subject(SystemEmail::VerifyEmail, "fr"),
            "Verify your email",
            "a locale without a translation falls back to English"
        );
    }

    #[test]
    fn every_system_subject_is_translated_in_the_shipped_locales() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");

        for email in SystemEmail::ALL {
            for locale in ["en", "de"] {
                assert_ne!(
                    renderer.subject(email, locale),
                    email.subject_key(),
                    "{email:?} has no {locale} subject"
                );
            }
        }
    }

    #[test]
    fn a_config_dir_translation_overrides_a_subject() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("translations");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("de.json"),
            r#"{ "email.subject.mfa_code": "Ihr Anmeldecode" }"#,
        )
        .unwrap();

        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");

        assert_eq!(
            renderer.subject(SystemEmail::MfaCode, "de"),
            "Ihr Anmeldecode"
        );
    }

    /// Regression: a subject override spanning lines loaded fine and then
    /// failed every send of that email (the header-injection guard refuses
    /// it). The renderer refuses it at start.
    #[test]
    fn a_multi_line_subject_override_fails_the_start() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("translations");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("fr.json"),
            r#"{ "email.subject.password_reset": "Réinitialiser\nBcc: x@y.z" }"#,
        )
        .unwrap();

        let err = EmailRenderer::new(tmp.path())
            .err()
            .expect("a multi-line subject is refused");

        assert!(
            format!("{err:#}").contains("email.subject.password_reset"),
            "{err:#}"
        );
    }

    #[test]
    fn renderer_new_loads_compiled_templates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        let result = renderer.render(
            "password_reset",
            &PasswordResetEmailContext {
                reset_url: "http://example.com/reset?token=abc",
                expiry_minutes: 30,
                from_name: "Test",
            },
        );
        assert!(result.is_ok());
        let html = result.unwrap();
        assert!(html.contains("reset") || html.contains("password"));
    }

    /// Interpolated values must not be glued to the surrounding words —
    /// "expires in60minutes" shipped for three releases because the only
    /// assertion was that the sender name appeared somewhere.
    #[test]
    fn rendered_text_keeps_spaces_around_interpolations() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");

        let html = renderer
            .render(
                "password_reset",
                &PasswordResetEmailContext {
                    reset_url: "http://example.com/reset?token=abc",
                    expiry_minutes: 60,
                    from_name: "Acme",
                },
            )
            .expect("render");

        assert!(
            html.contains("expires in 60 minutes"),
            "expiry sentence reads as prose: {html}"
        );
        assert!(
            html.contains("Sent by Acme"),
            "footer reads as prose: {html}"
        );
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
                &PasswordResetEmailContext {
                    reset_url: "http://example.com/reset",
                    expiry_minutes: 30,
                    from_name: "Test",
                },
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
            &PasswordResetEmailContext {
                reset_url: "http://example.com/reset",
                expiry_minutes: 30,
                from_name: "Test",
            },
        );
        assert!(result.is_ok());
    }

    /// Regression: a context missing the variables the template names used to
    /// render as empty strings, so a password-reset mail went out with no link
    /// while the token it carried was already consumed. The render fails now,
    /// which reports the send as failed.
    #[test]
    fn renderer_render_empty_data_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");

        assert!(
            renderer.render("password_reset", &json!({})).is_err(),
            "a context without the template's variables must not render"
        );
    }

    /// The same guard through the surface operators actually touch: an
    /// overlay template that misspells a variable fails the render instead of
    /// quietly dropping the link.
    #[test]
    fn renderer_overlay_with_an_unknown_variable_fails_the_render() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let email_dir = tmp.path().join("templates/email");
        std::fs::create_dir_all(&email_dir).unwrap();
        std::fs::write(
            email_dir.join("password_reset.hbs"),
            "<p>Reset: {{{reset_urls}}}</p>",
        )
        .unwrap();

        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        let err = renderer
            .render(
                "password_reset",
                &PasswordResetEmailContext {
                    reset_url: "http://example.com/reset",
                    expiry_minutes: 30,
                    from_name: "Test",
                },
            )
            .expect_err("a misspelled variable must fail the render");

        assert!(
            format!("{err:#}").contains("password_reset"),
            "the error should name the template: {err:#}"
        );
    }

    /// Regression: `verify_email.hbs` references `{{from_name}}`, and earlier the
    /// only call site (`service::email::send_verification_email`) forgot to pass
    /// it — handlebars non-strict mode silently rendered an empty string, leaving
    /// the footer reading "Sent by". Typed `VerifyEmailContext` makes that field
    /// mandatory; this test pins that the rendered output actually includes it.
    #[test]
    fn verify_email_renders_from_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let renderer = EmailRenderer::new(tmp.path()).expect("create renderer");
        let html = renderer
            .render(
                "verify_email",
                &VerifyEmailContext {
                    verify_url: "http://example.com/verify?token=abc",
                    from_name: "Acme",
                },
            )
            .expect("render");
        assert!(html.contains("Acme"));
    }
}
