//! GET /admin/mfa — show the MFA (Multi-Factor Authentication) code entry form.

use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};

use crate::admin::{
    AdminState,
    handlers::{
        auth::{append_cookies, clear_mfa_pending_cookie, extract_mfa_token, render_mfa},
        shared::paths,
    },
};

/// GET /admin/mfa — show the MFA code entry form. For `mfa = "totp"` the
/// pending token is validated here so the page can resolve the user's
/// enrollment state (and show the provisioning link on first setup).
pub async fn mfa_page(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    // If there's no pending MFA cookie, redirect to login.
    let Some(token) = extract_mfa_token(&headers) else {
        return Redirect::to(paths::LOGIN).into_response();
    };

    let Ok(claims) = state.infra.token_provider.validate_pending_token(&token) else {
        // Expired/invalid pending token: clear the cookie, back to login.
        let cookie = clear_mfa_pending_cookie(state.config.admin.dev_mode);
        let mut response = Redirect::to(paths::LOGIN).into_response();

        append_cookies(&mut response, &[cookie]);

        return response;
    };

    render_mfa(&state, &claims, None).await
}
