//! Error-page renderers shared between the auth middleware and
//! the admin-access gate. Both pages are last-resort templates
//! reachable when no principal is available (setup-required) or
//! when the user is authenticated but blocked by `admin.access`
//! (admin-denied); the `before_render` hook is skipped because
//! these aren't user-driven views.

use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use serde_json::{Value, to_value};

use crate::admin::{
    AdminState,
    context::{AuthBasePageContext, PageMeta, PageType},
};

/// Serialize a typed [`AuthBasePageContext`] for rendering. Avoids
/// the `before_render` hook (these are last-resort error pages
/// where the hook would just add noise).
fn serialize_auth_base(state: &AdminState, page: PageMeta) -> Value {
    to_value(AuthBasePageContext::for_state(state, page))
        .expect("AuthBasePageContext serializes infallibly")
}

/// Render the "setup required" page (no auth collection exists,
/// `require_auth` is on).
pub(super) fn auth_required_response(state: &AdminState) -> Response {
    let data = serialize_auth_base(
        state,
        PageMeta::new(PageType::AuthRequired, "error_auth_required_title"),
    );

    match state.render("errors/auth_required", &data) {
        Ok(html) => (StatusCode::SERVICE_UNAVAILABLE, Html(html)).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Setup required: no auth collection configured",
        )
            .into_response(),
    }
}

/// Render the "access denied" page (user authenticated but not
/// authorized for admin).
pub(super) fn admin_denied_response(state: &AdminState) -> Response {
    let data = serialize_auth_base(
        state,
        PageMeta::new(PageType::AdminDenied, "error_admin_access_denied_title"),
    );

    match state.render("errors/admin_denied", &data) {
        Ok(html) => (StatusCode::FORBIDDEN, Html(html)).into_response(),
        Err(_) => (StatusCode::FORBIDDEN, "Access denied").into_response(),
    }
}
