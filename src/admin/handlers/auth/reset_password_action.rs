use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use tokio::task;
use tracing::error;

use crate::admin::handlers::shared::HxNav;
use crate::core::collection::Auth;
use crate::{
    admin::{
        AdminState,
        context::{AuthBasePageContext, PageMeta, PageType, page::auth::ResetPasswordPage},
        handlers::{
            auth::{ResetPasswordForm, client_ip, scoped_limiter},
            shared::{paths, render_page},
        },
    },
    core::{Registry, SharedInvalidationTransport, auth::ResetTokenError},
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

    render_page(state, HxNav::full(), "auth/reset_password", &ctx)
}

/// Find the reset token across all auth collections, validate it, and update the password.
///
/// Searches every auth collection (with local auth enabled) for the token.
/// On success the password is updated and the token cleared inside a transaction.
///
/// On success the user's open live-update streams are torn down POST-COMMIT (a
/// password reset is a privilege-revoking action and an already-connected stream
/// never makes another request to pick up the bumped `_session_version`).
/// Publishing after commit ensures a rolled-back reset never spuriously tears
/// down a stream.
fn consume_reset_token(
    pool: &DbPool,
    registry: &Registry,
    token: &str,
    password: &str,
    invalidation_transport: &SharedInvalidationTransport,
) -> anyhow::Result<()> {
    let mut conn = pool.write()?;
    // SELECT-then-UPDATE (find token row, then write the new hash): take a write
    // lock up front. A DEFERRED tx would risk `SQLITE_BUSY_SNAPSHOT` under
    // concurrent writers — same reasoning as the gRPC reset path.
    let tx = conn.transaction_immediate()?;

    for def in registry.collections.values() {
        if !def.is_auth_collection() {
            continue;
        }

        if !def.auth.as_ref().is_some_and(Auth::password_login_enabled) {
            continue;
        }

        let ctx = ServiceContext::collection(&def.slug, def).conn(&tx).build();

        match service_consume_reset_token(&ctx, token, password) {
            Ok(user_id) => {
                tx.commit()?;
                // Tear down the user's open live-update streams POST-COMMIT.
                ServiceContext::slug_only(&def.slug)
                    .invalidation_transport(Some(invalidation_transport.clone()))
                    .build()
                    .publish_user_invalidation(&user_id);
                return Ok(());
            }
            Err(ServiceError::InvalidToken {
                reason: "not found",
                ..
            }) => {}
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

    // Local-only validation runs FIRST, before the rate-limit gate: a
    // mismatched confirmation or a policy violation is a legitimate user typo,
    // not a token guess, so it must not consume the IP's budget.
    if form.password != form.password_confirm {
        return render_reset_error(&state, Some(&form.token), "error_passwords_no_match");
    }

    if let Err(e) = state.config.auth.password_policy.validate(&form.password) {
        return render_reset_error(&state, Some(&form.token), &e.to_string());
    }

    // Rate limit by IP to prevent brute-forcing reset tokens. Atomically record
    // this attempt and bail if it puts the IP over the threshold — one backend
    // op, closing the check-then-record race the old `is_blocked` +
    // `record_failure` split left open. The gate sits AFTER the local checks
    // above so only genuine token-consumption attempts count. Every such attempt
    // counts (the same idiom as login and forgot-password), so a transient
    // internal error counts too — acceptable for a high-entropy token endpoint
    // and strictly safer than refunding. Uses its OWN per-IP keyspace (not the
    // shared forgot-password limiter) so reset-token attempts and the
    // forgot-password request flow don't drain each other's budget.
    let ip_reset_limiter = scoped_limiter(
        &state,
        "ip_reset_password",
        state.config.auth.max_ip_login_attempts,
        state.config.auth.forgot_password_window_seconds,
    );
    if ip_reset_limiter.check_and_block(&ip) {
        return render_reset_error(&state, Some(&form.token), "error_reset_link_invalid");
    }

    let pool = state.infra.pool.clone();
    let registry = Arc::clone(&state.infra.registry);
    let token = form.token.clone();
    let password = form.password.clone();
    let invalidation_transport = state.infra.invalidation_transport.clone();

    let result = task::spawn_blocking(move || {
        consume_reset_token(&pool, &registry, &token, &password, &invalidation_transport)
    })
    .await;

    match result {
        Ok(Ok(())) => {
            Redirect::to(&paths::login_with_success("success_password_reset")).into_response()
        }
        Ok(Err(e)) => {
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
