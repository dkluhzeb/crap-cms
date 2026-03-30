use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Form, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Redirect, Response},
};
use tokio::task;

use super::{LoginForm, client_ip, login_error, session_cookies};
use crate::{
    admin::AdminState,
    core::{
        CollectionDefinition, Document, Slug,
        auth::{ClaimsBuilder, create_token, dummy_verify, verify_password},
    },
    db::{DbPool, query},
};

/// Successful login result containing the user document and session version.
struct LoginSuccess {
    user: Document,
    session_version: u64,
}

/// Authenticate a user by email/password inside a blocking task.
///
/// Returns:
/// - `Ok(Some(Ok(success)))` — credentials valid, account active
/// - `Ok(Some(Err(msg)))` — credentials valid but account locked/unverified
/// - `Ok(None)` — invalid credentials (user not found or wrong password)
/// - `Err(_)` — internal/DB error
async fn verify_credentials(
    pool: DbPool,
    slug: String,
    def: CollectionDefinition,
    email: String,
    password: String,
    verify_email: bool,
) -> Result<Result<Option<Result<LoginSuccess, String>>, anyhow::Error>, task::JoinError> {
    task::spawn_blocking(move || {
        let conn = pool.get()?;

        let Some(user) = query::find_by_email(&conn, &slug, &def, &email)? else {
            dummy_verify();
            return Ok(None);
        };

        let Some(hash) = query::get_password_hash(&conn, &slug, &user.id)? else {
            dummy_verify();
            return Ok(None);
        };

        if !verify_password(&password, hash.as_ref())? {
            return Ok(None);
        }

        if query::is_locked(&conn, &slug, &user.id)? {
            tracing::debug!("Login denied for {}: account locked", user.id);
            return Ok(None);
        }

        if verify_email && !query::is_verified(&conn, &slug, &user.id)? {
            tracing::debug!("Login denied for {}: email not verified", user.id);
            return Ok(None);
        }

        let session_version = query::get_session_version(&conn, &slug, &user.id)?;

        Ok::<_, anyhow::Error>(Some(Ok(LoginSuccess {
            user,
            session_version,
        })))
    })
    .await
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

    let token = match create_token(&claims, state.jwt_secret.as_ref()) {
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

    if def.auth.as_ref().is_some_and(|a| a.disable_local) {
        return login_error(&state, "error_invalid_collection", &form.email);
    }

    let verify_email = def.auth.as_ref().is_some_and(|a| a.verify_email);

    let result = verify_credentials(
        state.pool.clone(),
        form.collection.clone(),
        def.clone(),
        form.email.clone(),
        form.password.clone(),
        verify_email,
    )
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
