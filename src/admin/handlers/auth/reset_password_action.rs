use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::{Html, IntoResponse, Redirect, Response},
};
use chrono::Utc;
use serde_json::json;
use tokio::task;

use super::{ResetPasswordForm, client_ip};
use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
    },
    core::auth::ResetTokenError,
    db::query,
};

/// Render a reset password error page with the given error key and optional token.
fn render_reset_error(state: &AdminState, token: Option<&str>, error: &str) -> Response {
    let mut builder = ContextBuilder::auth(state)
        .page(PageType::AuthReset, "Reset Password")
        .set("error", json!(error));

    if let Some(t) = token {
        builder = builder.set("token", json!(t));
    }

    let data = builder.build();

    match state.render("auth/reset_password", &data) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Template render error: {}", e);
            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
                .into_response()
        }
    }
}

/// POST /admin/reset-password — validate token, update password, redirect to login.
pub async fn reset_password_action(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<ResetPasswordForm>,
) -> Response {
    let ip = client_ip(&headers, &addr, state.config.server.trust_proxy);

    // Rate limit by IP — prevents brute-forcing reset tokens
    if state.ip_login_limiter.is_blocked(&ip) {
        return render_reset_error(&state, Some(&form.token), "error_reset_link_invalid");
    }

    if form.password != form.password_confirm {
        return render_reset_error(&state, Some(&form.token), "error_passwords_no_match");
    }

    if let Err(e) = state.config.auth.password_policy.validate(&form.password) {
        return render_reset_error(&state, Some(&form.token), &e.to_string());
    }

    let pool = state.pool.clone();
    let registry = state.registry.clone();
    let token = form.token.clone();
    let password = form.password.clone();

    let result = task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut conn = pool.get()?;
        let tx = conn.transaction()?;

        // Search all auth collections for the token
        for def in registry.collections.values() {
            if !def.is_auth_collection() {
                continue;
            }
            if def.auth.as_ref().is_some_and(|a| a.disable_local) {
                continue;
            }

            if let Some((user, exp)) = query::find_by_reset_token(&tx, &def.slug, def, &token)? {
                if Utc::now().timestamp() >= exp {
                    query::clear_reset_token(&tx, &def.slug, &user.id)?;
                    tx.commit()?;
                    return Err(ResetTokenError::Expired.into());
                }

                // Update password and clear token
                query::update_password(&tx, &def.slug, &user.id, &password)?;
                query::clear_reset_token(&tx, &def.slug, &user.id)?;
                tx.commit()?;

                return Ok(());
            }
        }

        Err(ResetTokenError::NotFound.into())
    })
    .await;

    match result {
        Ok(Ok(())) => Redirect::to("/admin/login?success=success_password_reset").into_response(),
        Ok(Err(e)) => {
            // Record failure on invalid/expired token — not on success
            state.ip_login_limiter.record_failure(&ip);

            let msg = match e.downcast_ref::<ResetTokenError>() {
                Some(ResetTokenError::Expired) => "error_reset_link_expired",
                _ => "error_reset_link_invalid",
            };
            render_reset_error(&state, None, msg)
        }
        Err(e) => {
            tracing::error!("Reset password task error: {}", e);
            render_reset_error(&state, None, "error_internal")
        }
    }
}
