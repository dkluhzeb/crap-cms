use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::Response,
};

use crate::{
    admin::{
        AdminState,
        handlers::auth::{
            ForgotPasswordForm, client_ip, get_auth_collections, render_forgot_success,
        },
    },
    core::{CollectionDefinition, collection::Auth, normalize_email},
    service::ResetTarget,
};

/// Check whether the collection supports forgot-password.
fn forgot_password_collection(
    state: &AdminState,
    collection: &str,
) -> Option<Arc<CollectionDefinition>> {
    let def = state.infra.registry.get_collection(collection)?;

    if def.is_auth_collection()
        && def.auth.as_ref().is_some_and(Auth::forgot_password_enabled)
        && def.auth.as_ref().is_some_and(Auth::password_login_enabled)
    {
        Some(def.clone())
    } else {
        None
    }
}

/// POST /admin/forgot-password — look up user, generate token, send email.
/// Always shows success (don't leak whether email exists).
pub async fn forgot_password_action(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<ForgotPasswordForm>,
) -> Response {
    let auth_collections = get_auth_collections(&state);
    let ip = client_ip(&headers, &addr, &state.config.server);

    // Rate limit: prevent email/IP flooding. Atomically record this attempt
    // against both limiters and bail if either is now over threshold — one
    // operation per limiter, closing the concurrent-bypass race that the old
    // is_blocked + separate record split left open. Both are evaluated (not
    // short-circuited) so each counter advances every attempt. Returning the
    // generic success on a block leaks nothing — the response is always
    // "success" regardless of whether the email exists.
    // Key the per-email limiter on the address in its stored form so spelling
    // variants of one account can't each get a fresh flooding budget — the
    // account lookup compares that form, so the limiter must too.
    let email_key = normalize_email(&form.email);

    let email_blocked = state.forgot_password_limiter.check_and_block(&email_key);
    let ip_blocked = state.ip_forgot_password_limiter.check_and_block(&ip);
    if email_blocked || ip_blocked {
        return render_forgot_success(&state, &auth_collections);
    }

    // Resolved before the form is consumed below.
    let target_def = forgot_password_collection(&state, &form.collection);

    if let Some(def) = target_def {
        // The token and the email job are minted together in one transaction
        // inside the spawned task, so a crash can never leave a live reset
        // token whose link was never queued for delivery.
        state.infra.email.send_reset(
            ResetTarget::builder(
                state.infra.pool.clone(),
                state.config.locale.clone(),
                form.collection,
                def,
                form.email,
                state.config.auth.reset_token_expiry,
            )
            .build(),
        );
    }

    render_forgot_success(&state, &auth_collections)
}
