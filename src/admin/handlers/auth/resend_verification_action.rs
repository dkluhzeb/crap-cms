use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};

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
        collection::Auth,
        login_email_key,
        rate_limit::{
            AttemptBudget, IP_RESEND_VERIFICATION_KEYSPACE, RESEND_VERIFICATION_KEYSPACE,
        },
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
    let client = client_ip(&headers, &addr, &state.config.server);

    // The response is the same success page whatever happens, so refusing an
    // address longer than any deliverable one — before it is keyed,
    // throttled or looked up — leaks nothing.
    let Some(email_key) = login_email_key(&form.email) else {
        return render_resend_verification(&state, &collections, true);
    };

    // Own keyspace, not the forgot-password limiters — matching what the
    // verify-email and reset-password routes already do. Sharing would let a
    // user who clicked "resend" three times lock themselves out of their own
    // password reset, and would let one NAT'd office IP do it for everyone
    // behind it.
    //
    // Derived from the forgot-password limiters with `rescoped`, exactly as
    // the gRPC twin does: the same thresholds and window, a separate budget.
    // Both surfaces build these the one way, so they cannot drift apart.
    // Recorded atomically, IP budget first (see `AttemptBudget`); a block
    // returns the same success page, so it leaks nothing.
    let email_limiter = state
        .forgot_password_limiter
        .rescoped(RESEND_VERIFICATION_KEYSPACE);
    let ip_limiter = state
        .ip_forgot_password_limiter
        .rescoped(IP_RESEND_VERIFICATION_KEYSPACE);

    if AttemptBudget::new(&ip_limiter, &email_limiter).check_and_block(&client, &email_key) {
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
