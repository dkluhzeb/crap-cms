use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::Response,
};
use tokio::task;
use tracing::error;

use crate::{
    admin::{
        AdminState, auth_middleware,
        handlers::auth::{
            LoginForm, client_ip, create_session_token, issue_mfa_challenge, login_error,
            refusal_error_key, session_redirect,
        },
        server::headers_to_map,
    },
    core::{
        CollectionDefinition, Document, SharedPasswordProvider,
        collection::{Auth, Surface},
        normalize_email,
    },
    service::{
        AppInfra, ServiceError,
        auth::{LoginFlowRequest, LoginOutcome, LoginVerified, SessionGrant, verify_login},
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
/// ([`verify_login`]) on the blocking pool — the same flow the
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

/// The address a session / MFA challenge for `user` carries: the stored
/// email, falling back to the one the login form submitted.
fn login_email(user: &Document, form: &LoginForm) -> String {
    user.fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or(&form.email)
        .to_string()
}

/// Issue the MFA challenge for a password-verified login; a refusal
/// re-renders the login form with its error.
fn handle_mfa_challenge(state: &AdminState, login: &LoginVerified, form: &LoginForm) -> Response {
    let email = login_email(&login.user, form);

    issue_mfa_challenge(state, &form.collection, login, &email)
        .unwrap_or_else(|refusal| login_error(state, refusal_error_key(refusal), &form.email))
}

/// Build the authenticated session response (JWT + cookies + redirect).
fn build_session_response(state: &AdminState, login: &LoginVerified, form: &LoginForm) -> Response {
    let user = &login.user;
    let user_email = login_email(user, form);

    let grant = SessionGrant::builder(
        &user.id,
        &form.collection,
        &user_email,
        login.session_version,
        Surface::Admin,
    )
    .mfa(login.mfa);

    let session = match create_session_token(state, grant) {
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
    // Key the per-email limiter on the address in its stored form (trimmed,
    // lowercased, NFC-composed) so spelling variants of the same account share
    // one bucket. The credential lookup compares that form, so without this an
    // attacker rotates `Victim@x.com` / `VICTIM@X.COM` / … to sidestep the
    // per-account lockout. The clear-on-success below uses the same key.
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
        return handle_mfa_challenge(&state, &login, &form);
    }

    build_session_response(&state, &login, &form)
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
