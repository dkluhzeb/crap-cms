use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use axum::{
    extract::{ConnectInfo, Form, State},
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use rand::Rng;
use tokio::task;
use tracing::{debug, error, warn};

use crate::core::collection::Auth;
use crate::hooks::lifecycle::AuthStrategyInput;
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
    config::EmailConfig,
    core::{
        CollectionDefinition, Document, DocumentId, SharedPasswordProvider, Slug, auth,
        auth::{ClaimsBuilder, TokenUse},
        collection::MfaMode,
        email::{self, EmailRenderer, MfaCodeEmailContext},
        normalize_email,
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
    allows_password: bool,
    hook_runner: Option<HookRunner>,
    headers: HashMap<String, String>,
}

/// Try external auth strategies via Lua hooks. Returns the first successful
/// match, or `None` if all strategies fail.
fn try_strategy_auth(
    conn: &BoxedConnection,
    def: &CollectionDefinition,
    hook_runner: &HookRunner,
    strategy_input: &AuthStrategyInput,
) -> Option<Document> {
    let auth = def.auth.as_ref()?;

    for strategy in auth.strategies() {
        match hook_runner.run_auth_strategy(strategy.authenticate, strategy_input, conn) {
            Ok(Some(doc)) => return Some(doc),
            Ok(None) => {}
            Err(e) => {
                // Log and fall through to the next strategy. Operators need visibility
                // into strategy failures (DB errors, bad config, Lua panics) that
                // previously silenced themselves as "authentication failed".
                error!(
                    collection = strategy_input.collection,
                    strategy = strategy.authenticate.reference(),
                    error = ?e,
                    "Custom auth strategy returned an error; continuing to next strategy"
                );
            }
        }
    }

    None
}

/// Synchronous body of [`verify_credentials`], extracted so the
/// `spawn_blocking` call is a single fn invocation (CLAUDE.md).
fn verify_credentials_blocking(params: &VerifyParams) -> anyhow::Result<Option<LoginSuccess>> {
    let conn = params.pool.get()?;
    let slug = &params.slug;
    let def = &params.def;

    // Try local email+password authentication via service layer
    if params.allows_password {
        let ctx = ServiceContext::collection(slug, def).conn(&conn).build();

        match authenticate_local(
            &ctx,
            &params.email,
            &params.password,
            &*params.password_provider,
            params.verify_email_flag,
        ) {
            Ok(result) => {
                return Ok(Some(LoginSuccess {
                    user: result.user,
                    session_version: result.session_version,
                }));
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

    // Fallback: try auth strategies if local auth failed/skipped. The submitted
    // credentials are exposed so a strategy can verify them against an external
    // system; the forwarded client IP rides in `headers` (X-Forwarded-For).
    let strategy_input = AuthStrategyInput {
        collection: slug,
        headers: &params.headers,
        email: Some(&params.email),
        password: Some(&params.password),
        remote_addr: None,
    };
    if let Some(runner) = &params.hook_runner
        && let Some(user) = try_strategy_auth(&conn, def, runner, &strategy_input)
    {
        let ctx = ServiceContext::slug_only(slug).conn(&conn).build();

        // Strategy-authenticated users still need locked/verified checks.
        // A lookup failure must fail CLOSED (deny) — letting a locked
        // account in on a transient DB error is an auth bypass. Matches
        // the `get_session_version` call below.
        if service::auth::is_locked(&ctx, &user.id).map_err(ServiceError::into_anyhow)? {
            debug!("Login denied for {}: account locked", user.id);
            return Ok(None);
        }

        if params.verify_email_flag
            && !service::auth::is_verified(&ctx, &user.id).map_err(ServiceError::into_anyhow)?
        {
            debug!("Login denied for {}: email not verified", user.id);
            return Ok(None);
        }

        let session_version = service::auth::get_session_version(&ctx, &user.id)
            .map_err(crate::service::ServiceError::into_anyhow)?;
        return Ok(Some(LoginSuccess {
            user,
            session_version,
        }));
    }

    if params.allows_password {
        auth::dummy_verify();
    }

    Ok(None)
}

async fn verify_credentials(
    params: VerifyParams,
) -> Result<anyhow::Result<Option<LoginSuccess>>, task::JoinError> {
    task::spawn_blocking(move || verify_credentials_blocking(&params)).await
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
    email_max_attempts: u32,
}

/// Store a 6-digit MFA code in the DB and queue the verification email.
///
/// Runs inside `spawn_blocking`. Errors are logged but not propagated —
/// the caller has already redirected to the MFA page.
fn send_mfa_code(params: &MfaCodeParams, code: &str) {
    let conn = match params.pool.get() {
        Ok(c) => c,
        Err(e) => {
            error!("DB connection for MFA code: {}", e);
            return;
        }
    };

    // Saturate to 0 (immediate expiry) on the impossible overflow path —
    // never-expires is the wrong fallback for security-sensitive timeouts.
    let exp = Utc::now().timestamp() + i64::try_from(MFA_PENDING_EXPIRY).unwrap_or(0);

    let ctx = ServiceContext::slug_only(&params.slug).conn(&conn).build();

    if let Err(e) = service::auth::set_mfa_code(&ctx, &params.user_id, code, exp) {
        error!("Failed to store MFA code: {}", e);
        return;
    }

    let html = match params.email_renderer.render(
        "mfa_code",
        &MfaCodeEmailContext {
            code,
            expiry_minutes: MFA_PENDING_EXPIRY / 60,
            from_name: &params.email_config.from_name,
        },
    ) {
        Ok(h) => h,
        Err(e) => {
            error!("Failed to render MFA email: {}", e);
            return;
        }
    };

    if let Err(e) = email::queue_email(
        &conn,
        &email::EmailJobData {
            to: params.user_email.clone(),
            subject: "Your verification code".to_string(),
            html,
            text: None,
        },
        params.email_max_attempts,
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
        .exp((Utc::now().timestamp().max(0).cast_unsigned()).saturating_add(MFA_PENDING_EXPIRY))
        .session_version(session_version)
        .token_use(TokenUse::MfaPending)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            error!("MFA pending claims error: {}", e);
            return login_error(state, "error_internal", &form.email);
        }
    };

    let mfa_token = match state.infra.token_provider.create_token(&claims) {
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
        // Generate 6-digit code and queue email in background
        let code = format!("{:06}", rand::rng().random_range(0..1_000_000));
        let code_for_db = code.clone();

        let params = MfaCodeParams {
            pool: state.infra.pool.clone(),
            slug: form.collection.clone(),
            user_id: user.id.clone(),
            user_email,
            email_config: state.config.email.clone(),
            email_renderer: state.infra.email.email_renderer.clone(),
            email_max_attempts: state.config.jobs.system_email_max_attempts(),
        };

        task::spawn_blocking(move || send_mfa_code(&params, &code_for_db));
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
        .filter(crate::core::CollectionDefinition::is_auth_collection)
    else {
        return login_error(&state, "error_invalid_collection", &form.email);
    };

    let allows_password = def.auth.as_ref().is_some_and(Auth::password_login_enabled);
    let has_strategies = def.auth.as_ref().is_some_and(Auth::has_strategies);

    // If password login is off and no strategies, nothing can authenticate
    if !allows_password && !has_strategies {
        return login_error(&state, "error_invalid_collection", &form.email);
    }

    let verify_email = def.auth.as_ref().is_some_and(Auth::requires_verify_email);

    let result = verify_credentials(VerifyParams {
        pool: state.infra.pool.clone(),
        password_provider: state.password_provider.clone(),
        slug: form.collection.clone(),
        def: def.clone(),
        email: form.email.clone(),
        password: form.password.clone(),
        verify_email_flag: verify_email,
        allows_password,
        hook_runner: Some(state.infra.hook_runner.clone()),
        headers: headers_to_map(&headers),
    })
    .await;

    let login = match result {
        Ok(Ok(Some(success))) => success,
        Ok(Ok(None)) => {
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

    // Check if MFA is required
    let mfa_enabled = def.auth.as_ref().is_some_and(|a| a.mfa() == MfaMode::Email);

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
