//! Form and query parameter structs for auth handlers.

use std::fmt;

use serde::Deserialize;

/// Form data for the login page.
#[derive(Deserialize)]
pub struct LoginForm {
    /// The slug of the collection the user belongs to.
    pub collection: String,
    /// The user's email address.
    pub email: String,
    /// The user's password.
    pub password: String,
}

impl fmt::Debug for LoginForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginForm")
            .field("collection", &self.collection)
            .field("email", &self.email)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

/// Query parameters for the login page.
#[derive(Debug, Deserialize, Default)]
pub struct LoginPageQuery {
    /// Optional success message to display (e.g. after logout or reset).
    pub success: Option<String>,
}

/// Form data for the forgot password page.
#[derive(Debug, Deserialize)]
pub struct ForgotPasswordForm {
    /// The slug of the collection the user belongs to.
    pub collection: String,
    /// The user's email address.
    pub email: String,
}

/// Query parameters for the reset password page.
#[derive(Debug, Deserialize)]
pub struct ResetPasswordQuery {
    /// The reset token sent via email.
    pub token: String,
}

/// Form data for the reset password page.
#[derive(Deserialize)]
pub struct ResetPasswordForm {
    /// The reset token from the URL.
    pub token: String,
    /// The new password.
    pub password: String,
    /// Confirmation of the new password.
    pub password_confirm: String,
}

impl fmt::Debug for ResetPasswordForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResetPasswordForm")
            .field("token", &"[REDACTED]")
            .field("password", &"[REDACTED]")
            .field("password_confirm", &"[REDACTED]")
            .finish()
    }
}

/// Query parameters for the email verification page.
#[derive(Debug, Deserialize)]
pub struct VerifyEmailQuery {
    /// The verification token sent via email.
    pub token: String,
}

/// Form data for the MFA code verification.
#[derive(Deserialize)]
pub struct MfaForm {
    /// The 6-digit MFA code entered by the user.
    pub code: String,
}

impl fmt::Debug for MfaForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MfaForm")
            .field("code", &"[REDACTED]")
            .finish()
    }
}

/// Form data for saving the UI locale.
#[derive(Debug, Deserialize)]
pub struct LocaleForm {
    /// The selected locale identifier.
    pub locale: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_form_debug_redacts_password_but_keeps_identity() {
        let form = LoginForm {
            collection: "users".into(),
            email: "a@example.com".into(),
            password: "hunter2-secret".into(),
        };
        let dbg = format!("{form:?}");

        assert!(dbg.contains("users"));
        assert!(dbg.contains("a@example.com"));
        assert!(dbg.contains("[REDACTED]"));
        assert!(!dbg.contains("hunter2-secret"));
    }

    #[test]
    fn reset_password_form_debug_redacts_token_and_both_passwords() {
        let form = ResetPasswordForm {
            token: "tok-abc-secret".into(),
            password: "new-pw-secret".into(),
            password_confirm: "new-pw-secret".into(),
        };
        let dbg = format!("{form:?}");

        assert!(dbg.contains("[REDACTED]"));
        assert!(!dbg.contains("tok-abc-secret"));
        assert!(!dbg.contains("new-pw-secret"));
    }

    #[test]
    fn mfa_form_debug_redacts_code() {
        let dbg = format!(
            "{:?}",
            MfaForm {
                code: "123456".into()
            }
        );

        assert!(dbg.contains("[REDACTED]"));
        assert!(!dbg.contains("123456"));
    }
}
