use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use rand::Rng;
use serde_json::json;
use tokio::task;
use tracing::{debug, error};

use crate::{
    admin::{
        AdminState,
        handlers::auth::{
            LoginForm, append_cookies, client_ip, create_session_token, headers_to_map,
            login_error, mfa_pending_cookie, session_redirect,
        },
    },
    config::EmailConfig,
    core::{
        CollectionDefinition, Document, DocumentId, Slug,
        auth::{ClaimsBuilder, SharedPasswordProvider},
        collection::MfaMode,
        email,
        email::EmailRenderer,
    },
    db::{BoxedConnection, DbPool},
    hooks::HookRunner,
    service::{self, ServiceContext, ServiceError, auth::authenticate_local},
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
    hook_runner: Option<HookRunner>,
    headers: HashMap<String, String>,
}

/// Try external auth strategies via Lua hooks. Returns the first successful
/// match, or `None` if all strategies fail.
fn try_strategy_auth(
    conn: &BoxedConnection,
    slug: &str,
    def: &CollectionDefinition,
    hook_runner: &HookRunner,
    headers: &HashMap<String, String>,
) -> Option<Document> {
    let auth = def.auth.as_ref()?;

    for strategy in &auth.strategies {
        if let Ok(Some(doc)) =
            hook_runner.run_auth_strategy(&strategy.authenticate, slug, headers, conn)
        {
            return Some(doc);
        }
    }

    None
}

async fn verify_credentials(
    params: VerifyParams,
) -> Result<Result<Option<Result<LoginSuccess, String>>, anyhow::Error>, task::JoinError> {
    task::spawn_blocking(move || {
        let conn = params.pool.get()?;
        let slug = &params.slug;
        let def = &params.def;

        // Try local email+password authentication via service layer
        if !params.disable_local {
            let ctx = ServiceContext::collection(slug, def).conn(&conn).build();

            match authenticate_local(
                &ctx,
                &params.email,
                &params.password,
                &*params.password_provider,
                params.verify_email_flag,
            ) {
                Ok(result) => {
                    return Ok(Some(Ok(LoginSuccess {
                        user: result.user,
                        session_version: result.session_version,
                    })));
                }
                Err(ServiceError::AccountLocked) => {
                    debug!("Login denied: account locked");
                    return Ok(None);
                }
                Err(ServiceError::EmailNotVerified) => {
                    debug!("Login denied: email not verified");
                    return Ok(None);
                }
                Err(ServiceError::InvalidCredentials) => {}
                Err(e) => return Err(e.into_anyhow()),
            }
        }

        // Fallback: try auth strategies if local auth failed/skipped
        if let Some(runner) = &params.hook_runner
            && let Some(user) = try_strategy_auth(&conn, slug, def, runner, &params.headers)
        {
            let ctx = ServiceContext::slug_only(slug).conn(&conn).build();

            // Strategy-authenticated users still need locked/verified checks
            if service::auth::is_locked(&ctx, &user.id).unwrap_or(false) {
                debug!("Login denied for {}: account locked", user.id);
                return Ok(None);
            }

            if params.verify_email_flag
                && !service::auth::is_verified(&ctx, &user.id).unwrap_or(false)
            {
                debug!("Login denied for {}: email not verified", user.id);
                return Ok(None);
            }

            let session_version =
                service::auth::get_session_version(&ctx, &user.id).map_err(|e| e.into_anyhow())?;
            return Ok(Some(Ok(LoginSuccess {
                user,
                session_version,
            })));
        }

        if !params.disable_local {
            crate::core::auth::dummy_verify();
        }

        Ok::<_, anyhow::Error>(None)
    })
    .await
}

/// MFA pending token expiry in seconds (5 minutes).
const MFA_PENDING_EXPIRY: u64 = 300;

/// Everything needed to store the MFA code and send it by email.
struct MfaCodeParams {
    pool: DbPool,
    slug: String,
    user_id: DocumentId,
    user_email: String,
    email_config: EmailConfig,
    email_renderer: Arc<EmailRenderer>,
}

/// Store a 6-digit MFA code in the DB and queue the verification email.
///
/// Runs inside `spawn_blocking`. Errors are logged but not propagated —
/// the caller has already redirected to the MFA page.
fn send_mfa_code(params: MfaCodeParams, code: &str) {
    let conn = match params.pool.get() {
        Ok(c) => c,
        Err(e) => {
            error!("DB connection for MFA code: {}", e);
            return;
        }
    };

    let exp = Utc::now().timestamp() + MFA_PENDING_EXPIRY as i64;

    let ctx = ServiceContext::slug_only(&params.slug).conn(&conn).build();

    if let Err(e) = service::auth::set_mfa_code(&ctx, &params.user_id, code, exp) {
        error!("Failed to store MFA code: {}", e);
        return;
    }

    let html = match params.email_renderer.render(
        "mfa_code",
        &json!({
            "code": code,
            "expiry_minutes": MFA_PENDING_EXPIRY / 60,
            "from_name": params.email_config.from_name,
        }),
    ) {
        Ok(h) => h,
        Err(e) => {
            error!("Failed to render MFA email: {}", e);
            return;
        }
    };

    if let Err(e) = email::queue_email(
        &conn,
        &params.user_email,
        "Your verification code",
        &html,
        None,
        params.email_config.queue_retries + 1,
        &params.email_config.queue_name,
    ) {
        error!("Failed to queue MFA email: {}", e);
    }
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

    // Create a short-lived MFA pending token (5 min)
    let claims = match ClaimsBuilder::new(user.id.clone(), Slug::new(&form.collection))
        .email(user_email.clone())
        .exp((Utc::now().timestamp().max(0) as u64).saturating_add(MFA_PENDING_EXPIRY))
        .session_version(session_version)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            error!("MFA pending claims error: {}", e);
            return login_error(state, "error_internal", &form.email);
        }
    };

    let mfa_token = match state.token_provider.create_token(&claims) {
        Ok(t) => t,
        Err(e) => {
            error!("MFA pending token error: {}", e);
            return login_error(state, "error_internal", &form.email);
        }
    };

    // Generate 6-digit code and queue email in background
    let code = format!("{:06}", rand::rng().random_range(0..1_000_000));
    let code_for_db = code.clone();

    let params = MfaCodeParams {
        pool: state.pool.clone(),
        slug: form.collection.clone(),
        user_id: user.id.clone(),
        user_email,
        email_config: state.config.email.clone(),
        email_renderer: state.email_renderer.clone(),
    };

    task::spawn_blocking(move || send_mfa_code(params, &code_for_db));

    // Set MFA pending cookie and redirect to MFA page
    let cookie = mfa_pending_cookie(&mfa_token, state.config.admin.dev_mode);
    let mut response =
        Redirect::to(&format!("/admin/mfa?collection={}", form.collection)).into_response();

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
    ) {
        Ok(s) => s,
        Err(e) => {
            error!("{}", e);
            return login_error(state, "error_internal", &form.email);
        }
    };

    session_redirect(&session, state.config.admin.dev_mode)
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
        headers: headers_to_map(&headers),
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
            error!("Login error: {}", e);

            return login_error(&state, "error_internal", &form.email);
        }
        Err(e) => {
            error!("Login task error: {}", e);

            return login_error(&state, "error_internal", &form.email);
        }
    };

    // Successful login — clear rate limit state for both email and IP.
    // Without clearing IP, successful logins still accumulate toward the IP threshold,
    // eventually locking out all users behind that IP (e.g., shared NAT/VPN).
    state.login_limiter.clear(&form.email);
    state.ip_login_limiter.clear(&ip);

    // Check admin.access gate before issuing session — deny login entirely
    // if the user doesn't pass the gate function.
    if let Some(response) =
        crate::admin::auth_middleware::check_admin_gate_for_doc(&state, &login.user).await
    {
        return response;
    }

    // Check if MFA is required
    let mfa_enabled = def.auth.as_ref().is_some_and(|a| a.mfa == MfaMode::Email);

    if mfa_enabled {
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
