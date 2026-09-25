//! Subject lines of the built-in system emails.
//!
//! Each subject is a translation key, so it follows the recipient's admin UI
//! language and can be overridden per locale from
//! `<config_dir>/translations/<locale>.json` like any other admin string.

/// A built-in email the CMS sends on its own behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemEmail {
    /// The sign-up / resend email-verification link.
    VerifyEmail,
    /// The forgot-password reset link.
    PasswordReset,
    /// The `mfa = "email"` one-time login code.
    MfaCode,
}

impl SystemEmail {
    /// Every system email.
    pub const ALL: [Self; 3] = [Self::VerifyEmail, Self::PasswordReset, Self::MfaCode];

    /// The translation key holding this email's subject line.
    #[must_use]
    pub fn subject_key(self) -> &'static str {
        match self {
            Self::VerifyEmail => "email.subject.verify_email",
            Self::PasswordReset => "email.subject.password_reset",
            Self::MfaCode => "email.subject.mfa_code",
        }
    }
}
