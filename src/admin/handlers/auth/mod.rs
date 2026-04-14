//! Login/logout/forgot-password/reset-password/verify-email/MFA/callback handlers for the admin UI.

/// Auth callback handler for external auth (OAuth, SSO).
pub mod callback;
/// Handler for the forgot password form submission.
pub mod forgot_password_action;
/// Handler for the forgot password request page.
pub mod forgot_password_page;
/// Handler for the login form submission.
pub mod login_action;
/// Handler for the login page.
pub mod login_page;
/// Handler for logging out and clearing session cookies.
pub mod logout_action;
/// Handlers for MFA code entry and verification.
pub mod mfa;
/// Handler for the reset password form submission.
pub mod reset_password_action;
/// Handler for the reset password page.
pub mod reset_password_page;
/// Handler for saving the user's UI locale preference.
pub mod save_locale;
/// Handler for refreshing the current session.
pub mod session_refresh;
/// Handler for verifying a user's email address.
pub mod verify_email;

mod forms;
mod helpers;
mod session;

pub use callback::auth_callback;
pub use forgot_password_action::forgot_password_action;
pub use forgot_password_page::forgot_password_page;
pub use login_action::login_action;
pub use login_page::login_page;
pub use logout_action::logout_action;
pub use mfa::{mfa_page, verify_mfa_action};
pub use reset_password_action::reset_password_action;
pub use reset_password_page::reset_password_page;
pub use save_locale::save_locale;
pub use session_refresh::session_refresh;
pub use verify_email::verify_email;

pub use forms::{
    ForgotPasswordForm, LocaleForm, LoginForm, LoginPageQuery, MfaForm, MfaQuery,
    ResetPasswordForm, ResetPasswordQuery, VerifyEmailQuery,
};
pub(super) use helpers::{
    all_disable_local, client_ip, create_session_token, extract_user_email, find_auth_collection,
    get_auth_collections, headers_to_map, login_error, render_forgot_success, session_redirect,
    show_forgot_password,
};
pub(super) use session::{
    append_cookies, clear_mfa_pending_cookie, clear_session_cookies, mfa_pending_cookie,
    session_cookies, session_same_site,
};
