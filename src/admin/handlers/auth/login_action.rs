use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use tokio::task;
use tracing::{error, warn};

use crate::core::collection::{Auth, Surface};
use crate::{
    admin::{
        AdminState, auth_middleware,
        handlers::{
            auth::{
                LoginForm, append_cookies, client_ip, create_session_token, headers_to_map,
                login_error, mfa_pending_cookie, scoped_limiter, session_redirect,
            },
            shared::paths,
        },
    },
    core::{CollectionDefinition, Document, SharedPasswordProvider, normalize_email},
    service::{
        AppInfra, ServiceError,
        auth::{self, LoginFlowRequest, LoginOutcome, verify_login},
    },
};

/// Owned bundle for the login spawn-blocking body. Process-stable
/// dependencies come from the shared [`AppInfra`]; the rest is per-call.
struct VerifyParams {
    infra: Arc<AppInfra>,
    password_provider: SharedPasswordProvider,
    slug: String,
    def: Arc<CollectionDefinition>,
    email: String,
    password: String,
    remote_addr: String,
    headers: HashMap<String, String>,
}

/// Run the shared credential-verification flow
/// ([`service::auth::verify_login`]) on the blocking pool — the same flow the
/// gRPC login uses, so the two surfaces cannot drift.
async fn verify_credentials(
    params: VerifyParams,
) -> Result<Result<LoginOutcome, ServiceError>, task::JoinError> {
    task::spawn_blocking(move || {
        verify_login(
            &params.infra,
            &LoginFlowRequest {
                slug: &params.slug,
                def: &params.def,
                email: &params.email,
                password: &params.password,
                headers: &params.headers,
                remote_addr: Some(&params.remote_addr),
                surface: Surface::Admin,
                password_provider: &*params.password_provider,
            },
        )
    })
    .await
}

/// Generate a 6-digit MFA code, store it, send it by email, and redirect to the MFA page.
fn handle_mfa_challenge(
    state: &AdminState,
    user: &Document,
    form: &LoginForm,
    session_version: u64,
) -> Response {
    let user_email = user
        .fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or(&form.email)
        .to_string();

    // Create a short-lived MFA pending token (5 min) via the shared
    // chokepoint (same token the gRPC challenge flow issues).
    let mfa_token = match auth::mint_mfa_pending_token(
        &state.infra,
        &form.collection,
        user,
        &user_email,
        session_version,
    ) {
        Ok(t) => t,
        Err(e) => {
            error!("MFA pending token error: {}", e);
            return login_error(state, "error_internal", &form.email);
        }
    };

    // Throttle MFA-code EMAIL ISSUANCE per user. The login limiter is cleared
    // on each successful password, so without this a password-holder could loop
    // /admin/login to flood the victim's inbox with codes and rack up send cost.
    // When over budget we skip regenerating/sending — the previously issued code
    // stays valid — but still set the pending cookie so the flow isn't broken.
    let issue_blocked = scoped_limiter(
        state,
        "mfa_issue",
        state.config.auth.max_forgot_password_attempts,
        state.config.auth.forgot_password_window_seconds,
    )
    .check_and_block(user.id.as_ref());

    if issue_blocked {
        warn!(user = %user.id, "MFA code issuance throttled — reusing the prior code");
    } else {
        // Generate a 6-digit code, store it, and deliver it (built-in email
        // or the collection's `mfa_deliver` hook) in the background — the
        // shared body the gRPC challenge flow also uses.
        let code = auth::generate_mfa_code();
        let infra = Arc::clone(&state.infra);
        let slug = form.collection.clone();
        let user_owned = user.clone();

        task::spawn_blocking(move || {
            auth::deliver_mfa_code(&infra, &slug, &user_owned, &user_email, &code);
        });
    }

    // Set MFA pending cookie and redirect to MFA page
    let cookie = mfa_pending_cookie(&mfa_token, state.config.admin.dev_mode);
    let mut response = Redirect::to(&paths::mfa_with_collection(&form.collection)).into_response();

    append_cookies(&mut response, &[cookie]);

    response
}

/// Build the authenticated session response (JWT + cookies + redirect).
fn build_session_response(
    state: &AdminState,
    user: &Document,
    form: &LoginForm,
    session_version: u64,
) -> Response {
    let user_email = user
        .fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or(&form.email)
        .to_string();

    let session = match create_session_token(
        state,
        user.id.to_string(),
        &form.collection,
        user_email,
        session_version,
        Utc::now().timestamp().max(0).cast_unsigned(),
    ) {
        Ok(s) => s,
        Err(e) => {
            error!("{}", e);
            return login_error(state, "error_internal", &form.email);
        }
    };

    session_redirect(state, &session)
}

/// POST /admin/login — verify credentials, set cookie, redirect.
pub async fn login_action(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    let ip = client_ip(&headers, &addr, &state.config.server);

    // Atomically record this attempt against both the email and IP limiters
    // and reject if either is now over threshold. Performing the check and the
    // increment as one operation closes the burst race the old is_blocked +
    // later record_failure split left open (concurrent attempts all passing an
    // under-limit check before any recorded). Both are evaluated (not
    // short-circuited) so each counter advances every attempt; a successful
    // login clears both below.
    // Key the per-email limiter on the normalized (trimmed, lowercased) address
    // so casing/whitespace variants of the same account share one bucket. The
    // credential lookup is case-insensitive (`LOWER(email) = ?`), so without
    // this an attacker rotates `Victim@x.com` / `VICTIM@X.COM` / … to sidestep
    // the per-account lockout. The clear-on-success below uses the same key.
    let email_key = normalize_email(&form.email);

    let email_blocked = state.login_limiter.check_and_block(&email_key);
    let ip_blocked = state.ip_login_limiter.check_and_block(&ip);
    if email_blocked || ip_blocked {
        return login_error(&state, "error_too_many_attempts", &form.email);
    }

    let Some(def) = state
        .infra
        .registry
        .get_collection(&form.collection)
        .cloned()
        .filter(|d| d.is_auth_collection())
    else {
        return login_error(&state, "error_invalid_collection", &form.email);
    };

    let allows_password = def.auth.as_ref().is_some_and(Auth::password_login_enabled);
    let has_strategies = def.auth.as_ref().is_some_and(Auth::has_strategies);

    // If password login is off and no strategies, nothing can authenticate
    if !allows_password && !has_strategies {
        return login_error(&state, "error_invalid_collection", &form.email);
    }

    let result = verify_credentials(VerifyParams {
        infra: state.infra.clone(),
        password_provider: state.password_provider.clone(),
        slug: form.collection.clone(),
        def: def.clone(),
        email: form.email.clone(),
        password: form.password.clone(),
        remote_addr: ip.clone(),
        headers: headers_to_map(&headers),
    })
    .await;

    let outcome = match result {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(e)) => {
            error!("Login error: {}", e);

            return login_error(&state, "error_internal", &form.email);
        }
        Err(e) => {
            error!("Login task error: {}", e);

            return login_error(&state, "error_internal", &form.email);
        }
    };

    let (login, mfa_required) = match outcome {
        LoginOutcome::Verified(v) => (v, false),
        LoginOutcome::MfaRequired(v) => (v, true),
        LoginOutcome::Denied => {
            return login_error(&state, "error_invalid_credentials", &form.email);
        }
    };

    // Successful login — clear the per-email limiter (scoped to this account,
    // which just proved its identity). For the SHARED per-IP limiter, only
    // *refund* this one attempt rather than clearing every failure: a success
    // shouldn't accumulate toward the IP threshold (NAT/VPN friendliness), but
    // it must not wipe other accounts' failures from the same IP either — that
    // would let one valid account on a shared IP mask a brute-force of others.
    state.login_limiter.clear(&email_key);
    state.ip_login_limiter.refund(&ip);

    // Check admin.access gate before issuing session — deny login entirely
    // if the user doesn't pass the gate function.
    if let Some(response) = auth_middleware::check_admin_gate_for_doc(&state, &login.user).await {
        return response;
    }

    // MFA requirement is decided inside the shared flow.
    if mfa_required {
        return handle_mfa_challenge(&state, &login.user, &form, login.session_version);
    }

    build_session_response(&state, &login.user, &form, login.session_version)
}

#[cfg(test)]
mod tests {
    use std::{thread::sleep, time::Duration};

    use crate::core::rate_limit::LoginRateLimiter;

    #[test]
    fn ip_limiter_blocks_after_threshold() {
        let limiter = LoginRateLimiter::new(3, 60);
        let ip = "1.2.3.4";
        limiter.record_failure(ip);
        limiter.record_failure(ip);
        assert!(!limiter.is_blocked(ip));
        limiter.record_failure(ip);
        assert!(limiter.is_blocked(ip));
    }

    #[test]
    fn ip_and_email_limiters_independent() {
        let email_limiter = LoginRateLimiter::new(2, 60);
        let ip_limiter = LoginRateLimiter::new(3, 60);

        // Block email limiter
        email_limiter.record_failure("a@b.com");
        email_limiter.record_failure("a@b.com");
        assert!(email_limiter.is_blocked("a@b.com"));

        // IP limiter should not be blocked
        assert!(!ip_limiter.is_blocked("1.2.3.4"));

        // Block IP limiter
        ip_limiter.record_failure("1.2.3.4");
        ip_limiter.record_failure("1.2.3.4");
        ip_limiter.record_failure("1.2.3.4");
        assert!(ip_limiter.is_blocked("1.2.3.4"));

        // Different IP should not be blocked
        assert!(!ip_limiter.is_blocked("5.6.7.8"));
    }

    #[test]
    fn ip_limiter_window_expiry() {
        let limiter = LoginRateLimiter::new(2, 0);
        limiter.record_failure("1.2.3.4");
        limiter.record_failure("1.2.3.4");
        sleep(Duration::from_millis(10));
        assert!(!limiter.is_blocked("1.2.3.4"));
    }

    /// Regression: successful login must clear the IP rate limiter, not just the
    /// email limiter. Without this, users behind a shared IP (NAT/VPN) eventually
    /// get locked out even when logging in successfully.
    #[test]
    fn ip_limiter_cleared_on_success() {
        let ip_limiter = LoginRateLimiter::new(3, 60);
        let ip = "10.0.0.1";

        // Accumulate 2 failures (one below threshold)
        ip_limiter.record_failure(ip);
        ip_limiter.record_failure(ip);
        assert!(!ip_limiter.is_blocked(ip));

        // Simulate successful login clearing the IP limiter
        ip_limiter.clear(ip);

        // After clearing, 2 more failures should not trigger the block
        // (would have been 4 total without clear, exceeding threshold of 3)
        ip_limiter.record_failure(ip);
        ip_limiter.record_failure(ip);
        assert!(!ip_limiter.is_blocked(ip));
    }

    /// Regression: email and IP limiters must both be cleared on success.
    /// Verifies the coordinated clear pattern used in the login handler.
    #[test]
    fn both_limiters_cleared_on_success() {
        let email_limiter = LoginRateLimiter::new(2, 60);
        let ip_limiter = LoginRateLimiter::new(3, 60);
        let email = "user@example.com";
        let ip = "192.168.1.1";

        // Record failures on both
        email_limiter.record_failure(email);
        ip_limiter.record_failure(ip);
        ip_limiter.record_failure(ip);

        // Simulate successful login — clear both
        email_limiter.clear(email);
        ip_limiter.clear(ip);

        // Both should be unblocked even after more failures up to threshold
        email_limiter.record_failure(email);
        assert!(!email_limiter.is_blocked(email));

        ip_limiter.record_failure(ip);
        ip_limiter.record_failure(ip);
        assert!(!ip_limiter.is_blocked(ip));
    }
}
