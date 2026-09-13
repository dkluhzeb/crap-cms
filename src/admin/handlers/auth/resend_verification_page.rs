use axum::{
    extract::State,
    response::{IntoResponse, Redirect, Response},
};

use crate::admin::{
    AdminState,
    handlers::{
        auth::{get_verifying_collections, render_resend_verification, show_resend_verification},
        shared::paths,
    },
};

/// GET /admin/resend-verification — render the resend form.
///
/// With no collection requiring email verification, or no email transport
/// configured, the form could never do anything — it would accept an address
/// and report success while the send is skipped with a log warning. The route
/// sends the visitor back to the login page instead, on the same condition
/// that decides whether the login page links here at all.
pub async fn resend_verification_page(State(state): State<AdminState>) -> Response {
    if !show_resend_verification(&state) {
        return Redirect::to(paths::LOGIN).into_response();
    }

    render_resend_verification(&state, &get_verifying_collections(&state), false)
}
