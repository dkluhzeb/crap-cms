use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::Html,
};
use serde_json::json;
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::auth::{
            ForgotPasswordForm, client_ip, get_auth_collections, render_forgot_success,
        },
    },
    config::EmailConfig,
    core::{CollectionDefinition, email, email::EmailRenderer},
    db::DbPool,
    service::{ServiceContext, auth::generate_reset_token},
};

/// Everything needed to look up a user and send the password-reset email.
struct ResetEmailParams {
    pool: DbPool,
    slug: String,
    def: CollectionDefinition,
    user_email: String,
    email_config: EmailConfig,
    email_renderer: Arc<EmailRenderer>,
    base_url: String,
    reset_expiry: u64,
}

/// Check whether the collection supports forgot-password.
fn forgot_password_collection(
    state: &AdminState,
    collection: &str,
) -> Option<CollectionDefinition> {
    let def = state.registry.get_collection(collection)?;

    if def.is_auth_collection()
        && def.auth.as_ref().is_some_and(|a| a.forgot_password)
        && !def.auth.as_ref().is_some_and(|a| a.disable_local)
    {
        Some(def.clone())
    } else {
        None
    }
}

/// Look up the user, generate a reset token, and queue the reset email.
///
/// Runs inside `spawn_blocking`. Silently returns on any failure — the
/// handler always shows "success" to avoid leaking whether the email exists.
fn send_reset_email(params: ResetEmailParams) {
    let conn = match params.pool.get() {
        Ok(c) => c,
        Err(e) => {
            error!("DB connection for forgot password: {}", e);
            return;
        }
    };

    let ctx = ServiceContext::collection(&params.slug, &params.def)
        .conn(&conn)
        .build();

    let token_result = match generate_reset_token(&ctx, &params.user_email, params.reset_expiry) {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            error!("Forgot password error: {}", e);
            return;
        }
    };
    let token = &token_result.token;

    let reset_url = format!("{}/admin/reset-password?token={}", params.base_url, token);

    let html = match params.email_renderer.render(
        "password_reset",
        &json!({
            "reset_url": reset_url,
            "expiry_minutes": params.reset_expiry / 60,
            "from_name": params.email_config.from_name,
        }),
    ) {
        Ok(h) => h,
        Err(e) => {
            error!("Failed to render reset email: {}", e);
            return;
        }
    };

    if let Err(e) = email::queue_email(
        &conn,
        &params.user_email,
        "Reset your password",
        &html,
        None,
        params.email_config.queue_retries + 1,
        &params.email_config.queue_name,
    ) {
        error!("Failed to queue reset email: {}", e);
    }
}

/// POST /admin/forgot-password — look up user, generate token, send email.
/// Always shows success (don't leak whether email exists).
pub async fn forgot_password_action(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<ForgotPasswordForm>,
) -> Html<String> {
    let auth_collections = get_auth_collections(&state);
    let ip = client_ip(&headers, &addr, &state.config.server);

    // Rate limit: prevent email/IP flooding
    if state.forgot_password_limiter.is_blocked(&form.email)
        || state.ip_forgot_password_limiter.is_blocked(&ip)
    {
        return render_forgot_success(&state, &auth_collections);
    }

    // Record rate limit immediately to prevent concurrent request bypass.
    // Safe to record unconditionally — the response is always "success"
    // regardless of whether the email exists, so no information is leaked.
    state.forgot_password_limiter.record_failure(&form.email);
    state.ip_forgot_password_limiter.record_failure(&ip);

    if let Some(def) = forgot_password_collection(&state, &form.collection) {
        let params = ResetEmailParams {
            pool: state.pool.clone(),
            slug: form.collection.clone(),
            def,
            user_email: form.email.clone(),
            email_config: state.config.email.clone(),
            email_renderer: state.email_renderer.clone(),
            base_url: state.config.server.base_url(),
            reset_expiry: state.config.auth.reset_token_expiry,
        };

        task::spawn_blocking(move || send_reset_email(params));
    }

    render_forgot_success(&state, &auth_collections)
}
