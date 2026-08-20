//! POST /admin/mfa — verify the MFA (Multi-Factor Authentication) code and
//! complete login.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState,
        handlers::{
            auth::{
                MfaForm, append_cookies, clear_mfa_pending_cookie, client_ip, create_session_token,
                extract_mfa_token, render_mfa_form, session_redirect,
            },
            shared::paths,
        },
    },
    core::auth::Claims,
    db::DbPool,
    service::{self, ServiceContext},
};

/// Pull a connection from the pool and run MFA-code verification. Extracted so
/// the `spawn_blocking` body is a single named call.
fn verify_mfa_blocking(
    pool: &DbPool,
    slug: &str,
    user_id: &str,
    code: &str,
) -> anyhow::Result<bool> {
    let conn = pool.get()?;
    let ctx = ServiceContext::slug_only(slug).conn(&conn).build();
    service::auth::verify_mfa_code(&ctx, user_id, code)
        .map_err(crate::service::ServiceError::into_anyhow)
}

/// Build the final session response after successful MFA verification.
fn build_mfa_session_response(state: &AdminState, pending: &Claims) -> Response {
    let session = match create_session_token(
        state,
        pending.sub.to_string(),
        &pending.collection,
        pending.email.clone(),
        pending.session_version,
        Utc::now().timestamp().max(0).cast_unsigned(),
    ) {
        Ok(s) => s,
        Err(e) => {
            error!("MFA session: {}", e);
            return render_mfa_form(state, Some("error_internal"));
        }
    };

    let mut response = session_redirect(state, &session);

    append_cookies(
        &mut response,
        &[clear_mfa_pending_cookie(state.config.admin.dev_mode)],
    );

    response
}

/// POST /admin/mfa — verify the MFA code and complete login.
pub async fn verify_mfa_action(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<MfaForm>,
) -> Response {
    // Extract and validate the MFA pending token.
    let Some(mfa_token) = extract_mfa_token(&headers) else {
        return Redirect::to(paths::LOGIN).into_response();
    };

    let Ok(pending_claims) = state.token_provider.validate_pending_token(&mfa_token) else {
        // Token expired, invalid, or not an MFA-pending token (a full session
        // token can't be replayed here) — clear cookie, redirect to login.
        let cookie = clear_mfa_pending_cookie(state.config.admin.dev_mode);
        let mut response = Redirect::to(paths::LOGIN).into_response();

        append_cookies(&mut response, &[cookie]);

        return response;
    };

    let ip = client_ip(&headers, &addr, &state.config.server);
    let user_id = pending_claims.sub.to_string();

    // Throttle MFA code guessing. The 6-digit code lives in a 10^6 space behind
    // a reusable pending token, so an unthrottled endpoint is brute-forceable
    // within the pending window. Atomically record this attempt against the
    // per-user AND per-IP MFA limiters and bail if either is now over threshold.
    // These limiters are independent of the login limiter (which is cleared on
    // the successful password *before* the challenge is issued), so an attacker
    // who knows the password cannot reset the MFA budget by re-logging-in. Both
    // are evaluated (not short-circuited) so each records the attempt.
    let user_blocked = state.mfa_limiter.check_and_block(&user_id);
    let ip_blocked = state.ip_mfa_limiter.check_and_block(&ip);
    if user_blocked || ip_blocked {
        return render_mfa_form(&state, Some("error_mfa_too_many_attempts"));
    }

    // Verify the MFA code against the database.
    let pool = state.pool.clone();
    let slug = pending_claims.collection.to_string();
    let code = form.code.clone();

    let verify_result = {
        let user_id = user_id.clone();
        task::spawn_blocking(move || verify_mfa_blocking(&pool, &slug, &user_id, &code)).await
    };

    let verified = match verify_result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            error!("MFA verification error: {}", e);

            return render_mfa_form(&state, Some("error_internal"));
        }
        Err(e) => {
            error!("MFA verification task error: {}", e);

            return render_mfa_form(&state, Some("error_internal"));
        }
    };

    if !verified {
        return render_mfa_form(&state, Some("error_mfa_invalid_code"));
    }

    // MFA verified — login is now fully complete, so clear the MFA limiters
    // (per-user and per-IP), mirroring how a successful password clears the
    // login limiters.
    state.mfa_limiter.clear(&user_id);
    state.ip_mfa_limiter.clear(&ip);

    build_mfa_session_response(&state, &pending_claims)
}
