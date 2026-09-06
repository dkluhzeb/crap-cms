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
                extract_mfa_token, render_mfa, session_redirect,
            },
            shared::paths,
        },
    },
    core::auth::Claims,
    service::{self, auth::reload_authenticated_user},
};

/// Owned inputs for the second-factor verification `spawn_blocking` body.
struct VerifyMfaInput {
    infra: std::sync::Arc<service::AppInfra>,
    auth_secret: String,
    slug: String,
    user_id: String,
    code: String,
}

/// Run second-factor verification (TOTP or stored code, dispatched on the
/// collection's MFA mode). Extracted so the `spawn_blocking` body is a
/// single named call.
fn verify_mfa_blocking(input: &VerifyMfaInput) -> anyhow::Result<bool> {
    service::auth::verify_second_factor(
        &input.infra,
        &input.auth_secret,
        &input.slug,
        &input.user_id,
        &input.code,
    )
    .map_err(service::ServiceError::into_anyhow)
}

/// Build the final session response after successful MFA verification.
async fn build_mfa_session_response(state: &AdminState, pending: &Claims) -> Response {
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
            return render_mfa(state, pending, Some("error_internal")).await;
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

    let Ok(pending_claims) = state
        .infra
        .token_provider
        .validate_pending_token(&mfa_token)
    else {
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
        return render_mfa(&state, &pending_claims, Some("error_mfa_too_many_attempts")).await;
    }

    // Verify the second factor (TOTP or stored code, per the collection).
    let input = VerifyMfaInput {
        infra: std::sync::Arc::clone(&state.infra),
        auth_secret: AsRef::<str>::as_ref(&state.config.auth.secret).to_string(),
        slug: pending_claims.collection.to_string(),
        user_id: user_id.clone(),
        code: form.code.clone(),
    };

    let verify_result = task::spawn_blocking(move || verify_mfa_blocking(&input)).await;

    let verified = match verify_result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            error!("MFA verification error: {}", e);

            return render_mfa(&state, &pending_claims, Some("error_internal")).await;
        }
        Err(e) => {
            error!("MFA verification task error: {}", e);

            return render_mfa(&state, &pending_claims, Some("error_internal")).await;
        }
    };

    if !verified {
        return render_mfa(&state, &pending_claims, Some("error_mfa_invalid_code")).await;
    }

    // Re-resolve the user fail-closed before completing login: a lock / delete
    // / session-version bump inside the pending-MFA window must invalidate the
    // challenge. Mirrors the gRPC VerifyMfa path via the shared
    // `reload_authenticated_user`, so both surfaces refuse to complete login
    // for an account that changed under the challenge.
    let infra = std::sync::Arc::clone(&state.infra);
    let claims_for_load = pending_claims.clone();
    let reloaded =
        task::spawn_blocking(move || reload_authenticated_user(&infra, &claims_for_load)).await;
    if !matches!(reloaded, Ok(Some(_))) {
        return Redirect::to(paths::LOGIN).into_response();
    }

    // MFA verified — login is now fully complete, so clear the MFA limiters
    // (per-user and per-IP), mirroring how a successful password clears the
    // login limiters.
    state.mfa_limiter.clear(&user_id);
    state.ip_mfa_limiter.clear(&ip);

    build_mfa_session_response(&state, &pending_claims).await
}
