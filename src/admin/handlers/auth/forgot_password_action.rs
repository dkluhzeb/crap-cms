use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::Response,
};
use tokio::task;
use tracing::error;

use crate::core::collection::Auth;
use crate::{
    admin::{
        AdminState,
        handlers::auth::{
            ForgotPasswordForm, client_ip, get_auth_collections, render_forgot_success,
        },
    },
    config::EmailConfig,
    core::{
        CollectionDefinition, email,
        email::{EmailRenderer, PasswordResetEmailContext},
        normalize_email,
    },
    db::DbPool,
    service::{ServiceContext, auth::generate_reset_token},
};

/// Everything needed to look up a user and send the password-reset email.
struct ResetEmailParams {
    pool: DbPool,
    slug: String,
    def: Arc<CollectionDefinition>,
    user_email: String,
    email_config: EmailConfig,
    email_renderer: Arc<EmailRenderer>,
    base_url: String,
    reset_expiry: u64,
    email_max_attempts: u32,
}

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

/// Look up the user, generate a reset token, and queue the reset email.
///
/// Runs inside `spawn_blocking`. Silently returns on any failure — the
/// handler always shows "success" to avoid leaking whether the email exists.
fn send_reset_email(params: &ResetEmailParams) {
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
        &PasswordResetEmailContext {
            reset_url: &reset_url,
            expiry_minutes: params.reset_expiry / 60,
            from_name: &params.email_config.from_name,
        },
    ) {
        Ok(h) => h,
        Err(e) => {
            error!("Failed to render reset email: {}", e);
            return;
        }
    };

    if let Err(e) = email::queue_email(
        &conn,
        &email::EmailJobData {
            to: params.user_email.clone(),
            subject: "Reset your password".to_string(),
            html,
            text: None,
        },
        params.email_max_attempts,
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
    // Normalize the per-email key (trim + lowercase) so casing variants of one
    // account can't each get a fresh flooding budget — the account lookup is
    // case-insensitive, so the limiter must be too.
    let email_key = normalize_email(&form.email);

    let email_blocked = state.forgot_password_limiter.check_and_block(&email_key);
    let ip_blocked = state.ip_forgot_password_limiter.check_and_block(&ip);
    if email_blocked || ip_blocked {
        return render_forgot_success(&state, &auth_collections);
    }

    if let Some(def) = forgot_password_collection(&state, &form.collection) {
        let params = ResetEmailParams {
            pool: state.infra.pool.clone(),
            slug: form.collection.clone(),
            def,
            user_email: form.email.clone(),
            email_config: state.config.email.clone(),
            email_renderer: state.infra.email.email_renderer.clone(),
            base_url: state.config.server.base_url(),
            reset_expiry: state.config.auth.reset_token_expiry,
            email_max_attempts: state.config.jobs.system_email_max_attempts(),
        };

        task::spawn_blocking(move || send_reset_email(&params));
    }

    render_forgot_success(&state, &auth_collections)
}
