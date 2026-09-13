use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};

use crate::core::collection::Auth;
use crate::{
    admin::{
        AdminState,
        handlers::{
            auth::{
                ResendVerificationForm, client_ip, get_verifying_collections,
                render_resend_verification, show_resend_verification,
            },
            shared::paths,
        },
    },
    core::{
        normalize_email,
        rate_limit::{IP_RESEND_VERIFICATION_KEYSPACE, RESEND_VERIFICATION_KEYSPACE},
    },
    service::ResendTarget,
};

/// POST /admin/resend-verification — issue a fresh verification link and mail
/// it.
///
/// Always renders the same success page. Whether the address belongs to an
/// unverified account, a verified one, a locked one, or no account at all,
/// the response is identical, so the form cannot be used to test which
/// addresses are registered.
pub async fn resend_verification_action(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<ResendVerificationForm>,
) -> Response {
    // Same gate as the login-page link: with nothing to verify, or with no
    // transport to send over, the page would promise a mail it cannot send.
    if !show_resend_verification(&state) {
        return Redirect::to(paths::LOGIN).into_response();
    }

    let collections = get_verifying_collections(&state);
    let ip = client_ip(&headers, &addr, &state.config.server);

    // Own keyspace, not the forgot-password limiters — matching what the
    // verify-email and reset-password routes already do. Sharing would let a
    // user who clicked "resend" three times lock themselves out of their own
    // password reset, and would let one NAT'd office IP do it for everyone
    // behind it.
    //
    // Recorded atomically against both limiters, and both are evaluated so
    // each counter advances on every attempt. Returning the generic success
    // on a block leaks nothing — the response is always the same. The
    // per-email key is normalized (trim + lowercase) because the account
    // lookup is case-insensitive, so casing variants must share a bucket.
    //
    // Derived from the forgot-password limiters with `rescoped`, exactly as
    // the gRPC twin does: the same thresholds and window, a separate budget.
    // Both surfaces build these the one way, so they cannot drift apart.
    let email_limiter = state
        .forgot_password_limiter
        .rescoped(RESEND_VERIFICATION_KEYSPACE);
    let ip_limiter = state
        .ip_forgot_password_limiter
        .rescoped(IP_RESEND_VERIFICATION_KEYSPACE);

    let email_key = normalize_email(&form.email);

    let email_blocked = email_limiter.check_and_block(&email_key);
    let ip_blocked = ip_limiter.check_and_block(&ip);
    if email_blocked || ip_blocked {
        return render_resend_verification(&state, &collections, true);
    }

    let requires_verification = state
        .infra
        .registry
        .get_collection(&form.collection)
        .filter(|def| def.is_auth_collection())
        .filter(|def| def.auth.as_ref().is_some_and(Auth::requires_verify_email))
        .cloned();

    if let Some(def) = requires_verification {
        state.infra.email.resend_verification(
            ResendTarget::builder(
                state.infra.pool.clone(),
                state.config.locale.clone(),
                form.collection,
                def,
                form.email,
            )
            .build(),
        );
    }

    render_resend_verification(&state, &collections, true)
}
