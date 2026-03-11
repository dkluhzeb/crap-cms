//! Login/logout/forgot-password/reset-password/verify-email handlers for the admin UI.

pub mod login_page;
pub mod login_action;
pub mod logout_action;
pub mod session_refresh;
pub mod forgot_password_page;
pub mod forgot_password_action;
pub mod reset_password_page;
pub mod reset_password_action;
pub mod verify_email;
pub mod save_locale;

pub use login_page::login_page;
pub use login_action::login_action;
pub use logout_action::logout_action;
pub use session_refresh::session_refresh;
pub use forgot_password_page::forgot_password_page;
pub use forgot_password_action::forgot_password_action;
pub use reset_password_page::reset_password_page;
pub use reset_password_action::reset_password_action;
pub use verify_email::verify_email;
pub use save_locale::save_locale;

// ── Shared structs and helpers ──────────────────────────────────────────────

use axum::response::{Html, IntoResponse};
use serde::Deserialize;

use crate::admin::context::{ContextBuilder, PageType};
use crate::admin::AdminState;
use crate::core::email;

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub collection: String,
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct LoginPageQuery {
    pub success: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ForgotPasswordForm {
    pub collection: String,
    pub email: String,
}

#[derive(Debug, Deserialize)]
pub struct ResetPasswordQuery {
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct ResetPasswordForm {
    pub token: String,
    pub password: String,
    pub password_confirm: String,
}

#[derive(Debug, Deserialize)]
pub struct VerifyEmailQuery {
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct LocaleForm {
    pub locale: String,
}

/// Build `Set-Cookie` header values for the session.
pub(super) fn session_cookies(token: &str, expiry: u64, exp: u64, dev_mode: bool) -> Vec<String> {
    let secure = if dev_mode { "" } else { "; Secure" };
    vec![
        format!(
            "crap_session={}; HttpOnly; Path=/; SameSite=Lax; Max-Age={}{}",
            token, expiry, secure,
        ),
        format!(
            "crap_session_exp={}; Path=/; SameSite=Lax; Max-Age={}{}",
            exp, expiry, secure,
        ),
    ]
}

/// Build `Set-Cookie` header values that clear both session cookies.
pub(super) fn clear_session_cookies(dev_mode: bool) -> Vec<String> {
    let secure = if dev_mode { "" } else { "; Secure" };
    vec![
        format!("crap_session=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0{}", secure),
        format!("crap_session_exp=; Path=/; SameSite=Lax; Max-Age=0{}", secure),
    ]
}

pub(super) fn login_error(state: &AdminState, error: &str, email: &str) -> axum::response::Response {
    let auth_collections = get_auth_collections(state);
    let all_disable_local = all_disable_local(state);
    let show_forgot_password = show_forgot_password(state);

    let data = ContextBuilder::auth(state)
        .page(PageType::AuthLogin, "Login")
        .set("error", serde_json::json!(error))
        .set("email", serde_json::json!(email))
        .set("collections", serde_json::json!(auth_collections))
        .set("show_collection_picker", serde_json::json!(auth_collections.len() > 1))
        .set("disable_local", serde_json::json!(all_disable_local))
        .set("show_forgot_password", serde_json::json!(show_forgot_password))
        .build();

    match state.render("auth/login", &data) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            }.into_response(),
    }
}

/// Check if all auth collections have disable_local = true.
pub(super) fn all_disable_local(state: &AdminState) -> bool {
    let auth_collections: Vec<_> = state.registry.collections.values()
        .filter(|def| def.is_auth_collection())
        .collect();
    if auth_collections.is_empty() {
        return false;
    }
    auth_collections.iter().all(|def| {
        def.auth.as_ref().map(|a| a.disable_local).unwrap_or(false)
    })
}

/// Check if "forgot password?" link should show on login page.
pub(super) fn show_forgot_password(state: &AdminState) -> bool {
    if !email::is_configured(&state.config.email) {
        return false;
    }
    state.registry.collections.values()
        .filter(|def| def.is_auth_collection())
        .any(|def| def.auth.as_ref().is_some_and(|a| a.forgot_password))
}

pub(super) fn get_auth_collections(state: &AdminState) -> Vec<serde_json::Value> {
    let mut collections: Vec<_> = state.registry.collections.values()
        .filter(|def| def.is_auth_collection())
        .map(|def| serde_json::json!({
            "slug": def.slug,
            "display_name": def.display_name(),
        }))
        .collect();
    collections.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));
    collections
}

pub(super) fn render_forgot_success(state: &AdminState, auth_collections: &[serde_json::Value]) -> Html<String> {
    let data = ContextBuilder::auth(state)
        .page(PageType::AuthForgot, "Forgot Password")
        .set("success", serde_json::json!(true))
        .set("collections", serde_json::json!(auth_collections))
        .set("show_collection_picker", serde_json::json!(auth_collections.len() > 1))
        .build();

    match state.render("auth/forgot_password", &data) {
        Ok(html) => Html(html),
        Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cookies_dev_mode() {
        let cookies = session_cookies("tok123", 7200, 1700000000, true);
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0].contains("crap_session=tok123"));
        assert!(cookies[0].contains("HttpOnly"));
        assert!(cookies[0].contains("Max-Age=7200"));
        assert!(!cookies[0].contains("Secure"));
        assert!(cookies[1].contains("crap_session_exp=1700000000"));
        assert!(!cookies[1].contains("HttpOnly"));
        assert!(cookies[1].contains("Max-Age=7200"));
        assert!(!cookies[1].contains("Secure"));
    }

    #[test]
    fn session_cookies_production_mode() {
        let cookies = session_cookies("tok456", 3600, 1700003600, false);
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0].contains("crap_session=tok456"));
        assert!(cookies[0].contains("Max-Age=3600"));
        assert!(cookies[0].contains("; Secure"));
        assert!(cookies[1].contains("crap_session_exp=1700003600"));
        assert!(!cookies[1].contains("HttpOnly"));
        assert!(cookies[1].contains("; Secure"));
    }

    #[test]
    fn clear_session_cookies_dev_mode() {
        let cookies = clear_session_cookies(true);
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0].contains("crap_session=;"));
        assert!(cookies[0].contains("Max-Age=0"));
        assert!(!cookies[0].contains("Secure"));
        assert!(cookies[1].contains("crap_session_exp=;"));
        assert!(cookies[1].contains("Max-Age=0"));
        assert!(!cookies[1].contains("HttpOnly"));
    }

    #[test]
    fn clear_session_cookies_production_mode() {
        let cookies = clear_session_cookies(false);
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0].contains("crap_session=;"));
        assert!(cookies[0].contains("Max-Age=0"));
        assert!(cookies[0].contains("; Secure"));
        assert!(cookies[1].contains("crap_session_exp=;"));
        assert!(cookies[1].contains("; Secure"));
    }
}
