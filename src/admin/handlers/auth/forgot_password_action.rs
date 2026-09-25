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
    core::{CollectionDefinition, collection::Auth, login_email_key, rate_limit::AttemptBudget},
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
    let client = client_ip(&headers, &addr, &state.config.server);

    // The response is the generic success whatever happens, so refusing an
    // address longer than any deliverable one — before it is keyed,
    // throttled or looked up — leaks nothing. The per-email key is the
    // address in its stored form, so spelling variants of one account share
    // one flooding budget.
    let Some(email_key) = login_email_key(&form.email) else {
        return render_forgot_success(&state, &auth_collections);
    };

    // Atomically record this attempt, IP budget first (see `AttemptBudget`),
    // and bail with the same generic success if either budget is spent.
    let budget = AttemptBudget::new(
        &state.ip_forgot_password_limiter,
        &state.forgot_password_limiter,
    );
    if budget.check_and_block(&client, &email_key) {
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
