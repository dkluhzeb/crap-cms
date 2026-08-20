//! GET /admin/mfa — show the MFA (Multi-Factor Authentication) code entry form.

use axum::{
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};

use crate::admin::{
    AdminState,
    handlers::{
        auth::{extract_mfa_token, render_mfa_form},
        shared::paths,
    },
};

/// GET /admin/mfa — show the MFA code entry form.
pub async fn mfa_page(State(state): State<AdminState>, headers: HeaderMap) -> Response {
    // If there's no pending MFA cookie, redirect to login.
    if extract_mfa_token(&headers).is_none() {
        return Redirect::to(paths::LOGIN).into_response();
    }

    render_mfa_form(&state, None)
}
