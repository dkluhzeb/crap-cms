use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        context::{AuthBasePageContext, PageMeta, PageType, page::auth::ResetPasswordPage},
        handlers::{
            auth::{ResetPasswordForm, client_ip},
            shared::render_page,
        },
    },
    core::{Registry, auth::ResetTokenError},
    db::DbPool,
    service::{
        ServiceContext, ServiceError, auth::consume_reset_token as service_consume_reset_token,
    },
};

/// Render a reset password error page with the given error key and optional token.
fn render_reset_error(state: &AdminState, token: Option<&str>, error: &str) -> Response {
    let ctx = ResetPasswordPage {
        base: AuthBasePageContext::for_state(
            state,
            PageMeta::new(PageType::AuthReset, "Reset Password"),
        ),
        token: token.map(str::to_string),
        error: Some(error.to_string()),
    };

    render_page(state, "auth/reset_password", &ctx)
}

/// Find the reset token across all auth collections, validate it, and update the password.
///
/// Searches every auth collection (with local auth enabled) for the token.
/// On success the password is updated and the token cleared inside a transaction.
fn consume_reset_token(
    pool: &DbPool,
    registry: &Registry,
    token: &str,
    password: &str,
) -> anyhow::Result<()> {
    let mut conn = pool.get()?;
    let tx = conn.transaction()?;

    for def in registry.collections.values() {
        if !def.is_auth_collection() {
            continue;
        }

        if def.auth.as_ref().is_some_and(|a| a.disable_local) {
            continue;
        }

        let ctx = ServiceContext::collection(&def.slug, def).conn(&tx).build();

        match service_consume_reset_token(&ctx, token, password) {
            Ok(()) => {
                tx.commit()?;
                return Ok(());
            }
            Err(ServiceError::InvalidToken {
                reason: "not found",
                ..
            }) => continue,
            Err(e) => {
                tx.commit()?;
                return Err(e.into_anyhow());
            }
        }
    }

    Err(ResetTokenError::NotFound.into())
}

/// POST /admin/reset-password — validate token, update password, redirect to login.
pub async fn reset_password_action(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<ResetPasswordForm>,
) -> Response {
    let ip = client_ip(&headers, &addr, &state.config.server);

    // Rate limit by IP — prevents brute-forcing reset tokens.
    // Uses the dedicated forgot-password IP limiter (not login limiter) to avoid
    // reset failures blocking legitimate login attempts from the same IP.
    if state.ip_forgot_password_limiter.is_blocked(&ip) {
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

    let result =
        task::spawn_blocking(move || consume_reset_token(&pool, &registry, &token, &password))
            .await;

    match result {
        Ok(Ok(())) => Redirect::to("/admin/login?success=success_password_reset").into_response(),
        Ok(Err(e)) => {
            // Record failure on invalid/expired token — not on success
            state.ip_forgot_password_limiter.record_failure(&ip);

            let msg = match e.downcast_ref::<ResetTokenError>() {
                Some(ResetTokenError::Expired) => "error_reset_link_expired",
                _ => "error_reset_link_invalid",
            };

            render_reset_error(&state, None, msg)
        }
        Err(e) => {
            error!("Reset password task error: {}", e);

            render_reset_error(&state, None, "error_internal")
        }
    }
}
