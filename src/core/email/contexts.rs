//! Typed contexts for built-in email templates.
//!
//! Each struct mirrors the variables used in the matching `templates/email/*.hbs`
//! file. Constructed at the call site, serialized by `EmailRenderer::render`.

use serde::Serialize;

/// Context for the `password_reset` email template.
#[derive(Serialize)]
pub struct PasswordResetEmailContext<'a> {
    pub reset_url: &'a str,
    pub expiry_minutes: u64,
    pub from_name: &'a str,
}

/// Context for the `verify_email` email template.
#[derive(Serialize)]
pub struct VerifyEmailContext<'a> {
    pub verify_url: &'a str,
    pub from_name: &'a str,
}

/// Context for the `mfa_code` email template.
#[derive(Serialize)]
pub struct MfaCodeEmailContext<'a> {
    pub code: &'a str,
    pub expiry_minutes: u64,
    pub from_name: &'a str,
}
