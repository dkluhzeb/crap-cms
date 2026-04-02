use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Form, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Redirect, Response},
};
use rand::Rng;
use serde_json::json;
use tokio::task;

use super::{LoginForm, client_ip, login_error, mfa_pending_cookie, session_cookies};
use crate::{
    admin::AdminState,
    core::{
        CollectionDefinition, Document, Slug,
        auth::{ClaimsBuilder, SharedPasswordProvider, dummy_verify},
        collection::MfaMode,
        email,
    },
    db::{DbPool, query},
};

/// Successful login result containing the user document and session version.
struct LoginSuccess {
    user: Document,
    session_version: u64,
}

struct VerifyParams {
    pool: DbPool,
    password_provider: SharedPasswordProvider,
    slug: String,
    def: CollectionDefinition,
    email: String,
    password: String,
    verify_email_flag: bool,
    disable_local: bool,
    hook_runner: Option<crate::hooks::HookRunner>,
    headers: std::collections::HashMap<String, String>,
}

async fn verify_credentials(
    params: VerifyParams,
) -> Result<Result<Option<Result<LoginSuccess, String>>, anyhow::Error>, task::JoinError> {
    task::spawn_blocking(move || {
        let conn = params.pool.get()?;
        let slug = &params.slug;
        let def = &params.def;

        // Try local email+password authentication unless disabled
        let mut user = None;

        if !params.disable_local {
            if let Some(doc) = query::find_by_email(&conn, slug, def, &params.email)? {
                let verified = match query::get_password_hash(&conn, slug, &doc.id)? {
                    Some(hash) => params
                        .password_provider
                        .verify_password(&params.password, hash.as_ref())?,
                    None => false,
                };

                if verified {
                    user = Some(doc);
                }
            } else {
                // User not found — burn CPU to prevent timing oracle
                dummy_verify();
            }
        }

        // Fallback: try auth strategies if local auth failed/skipped
        if user.is_none()
            && let Some(auth) = &def.auth
            && let Some(runner) = &params.hook_runner
        {
            for strategy in &auth.strategies {
                if let Ok(Some(doc)) =
                    runner.run_auth_strategy(&strategy.authenticate, slug, &params.headers, &conn)
                {
                    user = Some(doc);
                    break;
                }
            }
        }

        let user = match user {
            Some(doc) => doc,
            None => {
                if !params.disable_local {
                    dummy_verify();
                }
                return Ok(None);
            }
        };

        if query::is_locked(&conn, slug, &user.id)? {
            tracing::debug!("Login denied for {}: account locked", user.id);
            return Ok(None);
        }

        if params.verify_email_flag && !query::is_verified(&conn, slug, &user.id)? {
            tracing::debug!("Login denied for {}: email not verified", user.id);
            return Ok(None);
        }

        let session_version = query::get_session_version(&conn, slug, &user.id)?;

        Ok::<_, anyhow::Error>(Some(Ok(LoginSuccess {
            user,
            session_version,
        })))
    })
    .await
}

/// MFA pending token expiry in seconds (5 minutes).
const MFA_PENDING_EXPIRY: u64 = 300;

/// Generate a 6-digit MFA code, store it, send it by email, and redirect to the MFA page.
fn handle_mfa_challenge(
    state: &AdminState,
    user: &Document,
    form: &LoginForm,
    _def: &CollectionDefinition,
    session_version: u64,
) -> Response {
    let user_email = user
        .fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or(&form.email)
        .to_string();

    // Create a short-lived MFA pending token (5 min)
    let claims = match ClaimsBuilder::new(user.id.clone(), Slug::new(&form.collection))
        .email(user_email.clone())
        .exp((chrono::Utc::now().timestamp() as u64) + MFA_PENDING_EXPIRY)
        .session_version(session_version)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("MFA pending claims error: {}", e);
            return login_error(state, "error_internal", &form.email);
        }
    };

    let mfa_token = match state.token_provider.create_token(&claims) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("MFA pending token error: {}", e);
            return login_error(state, "error_internal", &form.email);
        }
    };

    // Generate 6-digit code and queue email in background
    let code = format!("{:06}", rand::rng().random_range(0..1_000_000));
    let pool = state.pool.clone();
    let slug = form.collection.clone();
    let user_id = user.id.clone();
    let code_for_db = code.clone();
    let email_config = state.config.email.clone();
    let email_renderer = state.email_renderer.clone();
    let expiry_minutes = MFA_PENDING_EXPIRY / 60;

    task::spawn_blocking(move || {
        let conn = match pool.get() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("DB connection for MFA code: {}", e);
                return;
            }
        };

        let exp = chrono::Utc::now().timestamp() + MFA_PENDING_EXPIRY as i64;

        if let Err(e) = query::set_mfa_code(&conn, &slug, &user_id, &code_for_db, exp) {
            tracing::error!("Failed to store MFA code: {}", e);
            return;
        }

        let html = match email_renderer.render(
            "mfa_code",
            &json!({
                "code": code_for_db,
                "expiry_minutes": expiry_minutes,
                "from_name": email_config.from_name,
            }),
        ) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("Failed to render MFA email: {}", e);
                return;
            }
        };

        if let Err(e) = email::queue_email(
            &conn,
            &user_email,
            "Your verification code",
            &html,
            None,
            email_config.queue_retries + 1,
            &email_config.queue_name,
        ) {
            tracing::error!("Failed to queue MFA email: {}", e);
        }
    });

    // Set MFA pending cookie and redirect to MFA page
    let cookie = mfa_pending_cookie(&mfa_token, state.config.admin.dev_mode);
    let mut response =
        Redirect::to(&format!("/admin/mfa?collection={}", form.collection)).into_response();

    response.headers_mut().append(
        header::SET_COOKIE,
        cookie.parse().expect("cookie header is valid ASCII"),
    );

    response
}

/// Build the authenticated session response (JWT + cookies + redirect).
fn build_session_response(
    state: &AdminState,
    user: &Document,
    form: &LoginForm,
    def: &CollectionDefinition,
    session_version: u64,
) -> Response {
    let user_email = user
        .fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or(&form.email)
        .to_string();

    let expiry = def
        .auth
        .as_ref()
        .map(|a| a.token_expiry)
        .unwrap_or(state.config.auth.token_expiry);

    let claims = match ClaimsBuilder::new(user.id.clone(), Slug::new(&form.collection))
        .email(user_email)
        .exp((chrono::Utc::now().timestamp() as u64) + expiry)
        .session_version(session_version)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Claims build error: {}", e);
            return login_error(state, "error_internal", &form.email);
        }
    };

    let token = match state.token_provider.create_token(&claims) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Token creation error: {}", e);
            return login_error(state, "error_internal", &form.email);
        }
    };

    let cookies = session_cookies(&token, expiry, claims.exp, state.config.admin.dev_mode);
    let mut response = Redirect::to("/admin").into_response();

    for cookie in cookies {
        response.headers_mut().append(
            header::SET_COOKIE,
            cookie.parse().expect("cookie header is valid ASCII"),
        );
    }

    response
}

/// POST /admin/login — verify credentials, set cookie, redirect.
pub async fn login_action(
    State(state): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Form(form): Form<LoginForm>,
) -> Response {
    let ip = client_ip(&headers, &addr, state.config.server.trust_proxy);

    // Check rate limits before doing any work (both email and IP)
    if state.login_limiter.is_blocked(&form.email) || state.ip_login_limiter.is_blocked(&ip) {
        return login_error(&state, "error_too_many_attempts", &form.email);
    }

    let Some(def) = state
        .registry
        .get_collection(&form.collection)
        .cloned()
        .filter(|d| d.is_auth_collection())
    else {
        return login_error(&state, "error_invalid_collection", &form.email);
    };

    let disable_local = def.auth.as_ref().is_some_and(|a| a.disable_local);
    let has_strategies = def.auth.as_ref().is_some_and(|a| !a.strategies.is_empty());

    // If local is disabled and no strategies, nothing can authenticate
    if disable_local && !has_strategies {
        return login_error(&state, "error_invalid_collection", &form.email);
    }

    let verify_email = def.auth.as_ref().is_some_and(|a| a.verify_email);

    // Extract headers for strategy functions
    let header_map: std::collections::HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_string())))
        .collect();

    let result = verify_credentials(VerifyParams {
        pool: state.pool.clone(),
        password_provider: state.password_provider.clone(),
        slug: form.collection.clone(),
        def: def.clone(),
        email: form.email.clone(),
        password: form.password.clone(),
        verify_email_flag: verify_email,
        disable_local,
        hook_runner: Some(state.hook_runner.clone()),
        headers: header_map,
    })
    .await;

    let login = match result {
        Ok(Ok(Some(Ok(success)))) => success,
        Ok(Ok(Some(Err(msg)))) => {
            state.login_limiter.record_failure(&form.email);
            state.ip_login_limiter.record_failure(&ip);
            return login_error(&state, &msg, &form.email);
        }
        Ok(Ok(None)) => {
            state.login_limiter.record_failure(&form.email);
            state.ip_login_limiter.record_failure(&ip);
            return login_error(&state, "error_invalid_credentials", &form.email);
        }
        Ok(Err(e)) => {
            tracing::error!("Login error: {}", e);
            return login_error(&state, "error_internal", &form.email);
        }
        Err(e) => {
            tracing::error!("Login task error: {}", e);
            return login_error(&state, "error_internal", &form.email);
        }
    };

    // Successful login — clear rate limit state for both email and IP.
    // Without clearing IP, successful logins still accumulate toward the IP threshold,
    // eventually locking out all users behind that IP (e.g., shared NAT/VPN).
    state.login_limiter.clear(&form.email);
    state.ip_login_limiter.clear(&ip);

    // Check if MFA is required
    let mfa_enabled = def.auth.as_ref().is_some_and(|a| a.mfa == MfaMode::Email);

    if mfa_enabled {
        return handle_mfa_challenge(&state, &login.user, &form, &def, login.session_version);
    }

    build_session_response(&state, &login.user, &form, &def, login.session_version)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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
        std::thread::sleep(Duration::from_millis(10));
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
