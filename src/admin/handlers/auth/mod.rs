//! Login/logout/forgot-password/reset-password/verify-email handlers for the admin UI.

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

pub use forgot_password_action::forgot_password_action;
pub use forgot_password_page::forgot_password_page;
pub use login_action::login_action;
pub use login_page::login_page;
pub use logout_action::logout_action;
pub use reset_password_action::reset_password_action;
pub use reset_password_page::reset_password_page;
pub use save_locale::save_locale;
pub use session_refresh::session_refresh;
pub use verify_email::verify_email;

pub use forms::{
    ForgotPasswordForm, LocaleForm, LoginForm, LoginPageQuery, ResetPasswordForm,
    ResetPasswordQuery, VerifyEmailQuery,
};
pub(super) use helpers::{
    all_disable_local, get_auth_collections, login_error, render_forgot_success,
    show_forgot_password,
};
pub(super) use session::{clear_session_cookies, session_cookies};
