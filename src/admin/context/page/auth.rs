//! Typed page contexts for the unauthenticated auth flow (login, MFA,
//! password reset, forgot password).

use schemars::JsonSchema;
use serde::Serialize;

use super::AuthBasePageContext;

/// One auth-enabled collection shown in the picker on the login / forgot
/// password forms (when more than one auth collection exists).
#[derive(Serialize, Clone, JsonSchema)]
pub struct AuthCollection {
    pub slug: String,
    pub display_name: String,
}

/// Login page context.
#[derive(Serialize, JsonSchema)]
pub struct LoginPage {
    #[serde(flatten)]
    pub base: AuthBasePageContext,

    /// Error key (e.g., `"error_invalid_credentials"`) — present after a
    /// failed login post.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Pre-fills the email field after a failed login.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,

    pub collections: Vec<AuthCollection>,
    pub show_collection_picker: bool,
    pub disable_local: bool,
    pub show_forgot_password: bool,

    /// Whitelisted success-message key shown after redirect from logout /
    /// email verification / password reset. Always emitted (as `null` when
    /// absent) to preserve the original `Option`-as-null contract.
    pub success: Option<String>,
}

/// MFA code entry page context.
#[derive(Serialize, JsonSchema)]
pub struct MfaPage {
    #[serde(flatten)]
    pub base: AuthBasePageContext,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Password-reset page (the form a user sees via the email link).
#[derive(Serialize, JsonSchema)]
pub struct ResetPasswordPage {
    #[serde(flatten)]
    pub base: AuthBasePageContext,

    /// Token from the URL — present only when valid. Absent when the link
    /// is bad / expired (in which case `error` is set instead).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Forgot-password page (the form where a user requests a reset email).
/// Renders the success state once the email has been queued.
#[derive(Serialize, JsonSchema)]
pub struct ForgotPasswordPage {
    #[serde(flatten)]
    pub base: AuthBasePageContext,

    pub success: bool,
    pub collections: Vec<AuthCollection>,
    pub show_collection_picker: bool,
}
