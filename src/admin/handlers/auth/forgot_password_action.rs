use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::Html,
};
use chrono::Utc;
use nanoid::nanoid;
use serde_json::json;
use tokio::task;

use super::{ForgotPasswordForm, client_ip, get_auth_collections, render_forgot_success};
use crate::{admin::AdminState, core::email, db::query};

/// POST /admin/forgot-password — look up user, generate token, send email.
/// Always shows success (don't leak whether email exists).
pub async fn forgot_password_action(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<ForgotPasswordForm>,
) -> Html<String> {
    let auth_collections = get_auth_collections(&state);
    let ip = client_ip(&headers, &addr, state.config.server.trust_proxy);

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

    // Try to find user and send reset email in background
    let def = state.registry.get_collection(&form.collection).cloned();

    if let Some(def) = def
        && def.is_auth_collection()
        && def.auth.as_ref().is_some_and(|a| a.forgot_password)
        && !def.auth.as_ref().is_some_and(|a| a.disable_local)
    {
        let pool = state.pool.clone();
        let slug = form.collection.clone();
        let user_email = form.email.clone();
        let def_owned = def;
        let email_config = state.config.email.clone();
        let public_url = state.config.server.public_url.clone();
        let admin_port = state.config.server.admin_port;
        let host = state.config.server.host.clone();
        let reset_expiry = state.config.auth.reset_token_expiry;

        // Load email renderer (we do this on the main thread since it's cheap)
        let email_renderer = state.email_renderer.clone();

        task::spawn_blocking(move || {
            let conn = match pool.get() {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("DB connection for forgot password: {}", e);
                    return;
                }
            };

            let user = match query::find_by_email(&conn, &slug, &def_owned, &user_email) {
                Ok(Some(u)) => u,
                Ok(None) => return, // Don't leak existence
                Err(e) => {
                    tracing::error!("Forgot password lookup: {}", e);
                    return;
                }
            };

            // Generate reset token (nanoid)
            let token = nanoid!();
            let exp = Utc::now().timestamp() + reset_expiry as i64;

            if let Err(e) = query::set_reset_token(&conn, &slug, &user.id, &token, exp) {
                tracing::error!("Failed to set reset token: {}", e);
                return;
            }

            // Send reset email
            let base_url = public_url.unwrap_or_else(|| {
                if host == "0.0.0.0" {
                    format!("http://localhost:{}", admin_port)
                } else {
                    format!("http://{}:{}", host, admin_port)
                }
            });
            let reset_url = format!("{}/admin/reset-password?token={}", base_url, token);

            let html = match email_renderer.render(
                "password_reset",
                &json!({
                    "reset_url": reset_url,
                    "expiry_minutes": reset_expiry / 60,
                    "from_name": email_config.from_name,
                }),
            ) {
                Ok(h) => h,
                Err(e) => {
                    tracing::error!("Failed to render reset email: {}", e);
                    return;
                }
            };

            if let Err(e) = email::queue_email(
                &conn,
                &user_email,
                "Reset your password",
                &html,
                None,
                email_config.queue_retries + 1,
                &email_config.queue_name,
            ) {
                tracing::error!("Failed to queue reset email: {}", e);
            }
        });
    }

    render_forgot_success(&state, &auth_collections)
}
