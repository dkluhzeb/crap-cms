//! Login/logout/forgot-password/reset-password/verify-email handlers for the admin UI.

use axum::{
    extract::{Form, Query, State},
    Extension,
    http::StatusCode,
    response::{Html, IntoResponse, Redirect},
};
use serde::Deserialize;

use crate::admin::context::{ContextBuilder, PageType};
use crate::admin::AdminState;
use crate::core::auth;
use crate::core::auth::ClaimsBuilder;
use crate::core::email;
use crate::db::query;

/// Form data submitted by the login page.
#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub collection: String,
    pub email: String,
    pub password: String,
}

/// GET /admin/login — render the login page.
pub async fn login_page(
    State(state): State<AdminState>,
    query: Query<LoginPageQuery>,
) -> Html<String> {
    let auth_collections = get_auth_collections(&state);
    let all_disable_local = all_disable_local(&state);
    let show_forgot_password = show_forgot_password(&state);

    let data = ContextBuilder::auth(&state)
        .page(PageType::AuthLogin, "Login")
        .set("collections", serde_json::json!(auth_collections))
        .set("show_collection_picker", serde_json::json!(auth_collections.len() > 1))
        .set("disable_local", serde_json::json!(all_disable_local))
        .set("show_forgot_password", serde_json::json!(show_forgot_password))
        .set("success", serde_json::json!(query.success.as_deref()))
        .build();

    let data = state.hook_runner.run_before_render(data);

    match state.render("auth/login", &data) {
        Ok(html) => Html(html),
        Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            },
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct LoginPageQuery {
    pub success: Option<String>,
}

/// POST /admin/login — verify credentials, set cookie, redirect.
pub async fn login_action(
    State(state): State<AdminState>,
    Form(form): Form<LoginForm>,
) -> axum::response::Response {
    // Check rate limit before doing any work
    if state.login_limiter.is_blocked(&form.email) {
        return login_error(&state, "error_too_many_attempts", &form.email);
    }

    let def = state.registry.get_collection(&form.collection).cloned();

    let def = match def {
        Some(d) if d.is_auth_collection() => d,
        _ => return login_error(&state, "error_invalid_collection", &form.email),
    };

    let pool = state.pool.clone();
    let slug = form.collection.clone();
    let email = form.email.clone();
    let password = form.password.clone();
    let def_owned = def.clone();
    let verify_email = def.auth.as_ref().is_some_and(|a| a.verify_email);

    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;

        // Find user by email
        let user = query::find_by_email(&conn, &slug, &def_owned, &email)?;
        let user = match user {
            Some(u) => u,
            None => { auth::dummy_verify(); return Ok(None); }
        };

        // Verify password
        let hash = query::get_password_hash(&conn, &slug, &user.id)?;
        let hash = match hash {
            Some(h) => h,
            None => { auth::dummy_verify(); return Ok(None); }
        };

        if !auth::verify_password(&password, &hash)? {
            return Ok(None);
        }

        // Check if account is locked
        if query::is_locked(&conn, &slug, &user.id)? {
            return Ok(Some(Err("error_account_locked".to_string())));
        }

        // Check email verification if enabled
        if verify_email {
            let verified = query::is_verified(&conn, &slug, &user.id)?;
            if !verified {
                return Ok(Some(Err("error_verify_email".to_string())));
            }
        }

        Ok::<_, anyhow::Error>(Some(Ok(user)))
    }).await;

    let user = match result {
        Ok(Ok(Some(Ok(user)))) => user,
        Ok(Ok(Some(Err(msg)))) => {
            state.login_limiter.record_failure(&form.email);
            return login_error(&state, &msg, &form.email);
        }
        Ok(Ok(None)) => {
            state.login_limiter.record_failure(&form.email);
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

    // Successful login — clear any rate limit state
    state.login_limiter.clear(&form.email);

    // Get email from user document
    let user_email = user.fields.get("email")
        .and_then(|v| v.as_str())
        .unwrap_or(&form.email)
        .to_string();

    // Determine token expiry
    let expiry = def.auth.as_ref()
        .map(|a| a.token_expiry)
        .unwrap_or(state.config.auth.token_expiry);

    // Create JWT
    let claims = ClaimsBuilder::new(&user.id, &form.collection)
        .email(user_email)
        .exp((chrono::Utc::now().timestamp() as u64) + expiry)
        .build();

    let token = match auth::create_token(&claims, &state.jwt_secret) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Token creation error: {}", e);
            return login_error(&state, "error_internal", &form.email);
        }
    };

    // Set cookies and redirect
    let cookies = session_cookies(&token, expiry, claims.exp, state.config.admin.dev_mode);

    let mut response = Redirect::to("/admin").into_response();
    for cookie in cookies {
        response.headers_mut().append(
            axum::http::header::SET_COOKIE,
            cookie.parse().expect("cookie header is valid ASCII"),
        );
    }
    response
}

/// GET/POST /admin/logout — clear cookies, redirect to login.
pub async fn logout_action(
    State(state): State<AdminState>,
) -> axum::response::Response {
    let cookies = clear_session_cookies(state.config.admin.dev_mode);
    let mut response = Redirect::to("/admin/login").into_response();
    for cookie in cookies {
        response.headers_mut().append(
            axum::http::header::SET_COOKIE,
            cookie.parse().expect("cookie header is valid ASCII"),
        );
    }
    response
}

/// POST /admin/api/session-refresh — issue a fresh JWT if the current one is still valid.
pub async fn session_refresh(
    State(state): State<AdminState>,
    request: axum::http::Request<axum::body::Body>,
) -> axum::response::Response {
    // Extract claims from request extensions (set by auth middleware)
    let claims = match request.extensions().get::<auth::Claims>() {
        Some(c) => c.clone(),
        None => return StatusCode::UNAUTHORIZED.into_response(),
    };

    // Check account is not locked
    let pool = state.pool.clone();
    let slug = claims.collection.clone();
    let user_id = claims.sub.clone();

    let locked = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        query::is_locked(&conn, &slug, &user_id)
    }).await;

    match locked {
        Ok(Ok(true)) => return StatusCode::UNAUTHORIZED.into_response(),
        Ok(Err(e)) => {
            tracing::error!("Session refresh lock check: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Err(e) => {
            tracing::error!("Session refresh task error: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        _ => {} // not locked, continue
    }

    // Compute fresh expiry (collection override or global config)
    let expiry = state.registry.get_collection(&claims.collection)
        .and_then(|def| def.auth.as_ref().map(|a| a.token_expiry))
        .unwrap_or(state.config.auth.token_expiry);

    let new_claims = ClaimsBuilder::new(claims.sub, claims.collection)
        .email(claims.email)
        .exp((chrono::Utc::now().timestamp() as u64) + expiry)
        .build();

    let token = match auth::create_token(&new_claims, &state.jwt_secret) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Session refresh token creation: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let cookies = session_cookies(&token, expiry, new_claims.exp, state.config.admin.dev_mode);
    let mut response = StatusCode::NO_CONTENT.into_response();
    for cookie in cookies {
        response.headers_mut().append(
            axum::http::header::SET_COOKIE,
            cookie.parse().expect("cookie header is valid ASCII"),
        );
    }
    response
}

// ── Forgot password ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ForgotPasswordForm {
    pub collection: String,
    pub email: String,
}

/// GET /admin/forgot-password — render the forgot password form.
pub async fn forgot_password_page(State(state): State<AdminState>) -> Html<String> {
    let auth_collections = get_auth_collections(&state);

    let data = ContextBuilder::auth(&state)
        .page(PageType::AuthForgot, "Forgot Password")
        .set("collections", serde_json::json!(auth_collections))
        .set("show_collection_picker", serde_json::json!(auth_collections.len() > 1))
        .build();

    let data = state.hook_runner.run_before_render(data);

    match state.render("auth/forgot_password", &data) {
        Ok(html) => Html(html),
        Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            },
    }
}

/// POST /admin/forgot-password — look up user, generate token, send email.
/// Always shows success (don't leak whether email exists).
pub async fn forgot_password_action(
    State(state): State<AdminState>,
    Form(form): Form<ForgotPasswordForm>,
) -> Html<String> {
    let auth_collections = get_auth_collections(&state);

    // Rate limit: prevent email flooding (always count, always return success)
    if state.forgot_password_limiter.is_blocked(&form.email) {
        return render_forgot_success(&state, &auth_collections);
    }
    state.forgot_password_limiter.record_failure(&form.email);

    // Try to find user and send reset email in background
    let def = state.registry.get_collection(&form.collection).cloned();

    if let Some(def) = def {
        if def.is_auth_collection() && def.auth.as_ref().is_some_and(|a| a.forgot_password) {
            let pool = state.pool.clone();
            let slug = form.collection.clone();
            let user_email = form.email.clone();
            let def_owned = def;
            let email_config = state.config.email.clone();
            let admin_port = state.config.server.admin_port;
            let host = state.config.server.host.clone();
            let reset_expiry = state.config.auth.reset_token_expiry;

            // Load email renderer (we do this on the main thread since it's cheap)
            let email_renderer = state.email_renderer.clone();

            tokio::task::spawn_blocking(move || {
                let conn = match pool.get() {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::error!("DB connection for forgot password: {}", e);
                        return;
                    }
                };

                let user = match query::find_by_email(&conn, &slug, &def_owned, &user_email) {
                    Ok(Some(u)) => u,
                    Ok(None) => return, // Don't leak existence
                    Err(e) => {
                        tracing::error!("Forgot password lookup: {}", e);
                        return;
                    }
                };

                // Generate reset token (nanoid)
                let token = nanoid::nanoid!();
                let exp = chrono::Utc::now().timestamp() + reset_expiry as i64;

                if let Err(e) = query::set_reset_token(&conn, &slug, &user.id, &token, exp) {
                    tracing::error!("Failed to set reset token: {}", e);
                    return;
                }

                // Send reset email
                let base_url = if host == "0.0.0.0" {
                    format!("http://localhost:{}", admin_port)
                } else {
                    format!("http://{}:{}", host, admin_port)
                };
                let reset_url = format!("{}/admin/reset-password?token={}", base_url, token);

                let html = match email_renderer.render("password_reset", &serde_json::json!({
                    "reset_url": reset_url,
                    "expiry_minutes": reset_expiry / 60,
                    "from_name": email_config.from_name,
                })) {
                    Ok(h) => h,
                    Err(e) => {
                        tracing::error!("Failed to render reset email: {}", e);
                        return;
                    }
                };

                if let Err(e) = email::send_email(&email_config, &user_email, "Reset your password", &html, None) {
                    tracing::error!("Failed to send reset email: {}", e);
                }
            });
        }
    }

    render_forgot_success(&state, &auth_collections)
}

fn render_forgot_success(state: &AdminState, auth_collections: &[serde_json::Value]) -> Html<String> {
    let data = ContextBuilder::auth(state)
        .page(PageType::AuthForgot, "Forgot Password")
        .set("success", serde_json::json!(true))
        .set("collections", serde_json::json!(auth_collections))
        .set("show_collection_picker", serde_json::json!(auth_collections.len() > 1))
        .build();

    match state.render("auth/forgot_password", &data) {
        Ok(html) => Html(html),
        Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            },
    }
}

// ── Reset password ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ResetPasswordQuery {
    pub token: String,
}

#[derive(Debug, Deserialize)]
pub struct ResetPasswordForm {
    pub token: String,
    pub password: String,
    pub password_confirm: String,
}

/// GET /admin/reset-password?token=xxx — validate token, show reset form.
pub async fn reset_password_page(
    State(state): State<AdminState>,
    Query(query): Query<ResetPasswordQuery>,
) -> Html<String> {
    // Validate the token exists and isn't expired
    let pool = state.pool.clone();
    let registry = state.registry.clone();
    let token = query.token.clone();

    let valid = tokio::task::spawn_blocking(move || {
        let conn = match pool.get() {
            Ok(c) => c,
            Err(_) => return false,
        };
        for def in registry.collections.values() {
            if !def.is_auth_collection() { continue; }
            match query::find_by_reset_token(&conn, &def.slug, def, &token) {
                Ok(Some((_, exp))) => {
                    return chrono::Utc::now().timestamp() < exp;
                }
                _ => continue,
            }
        }
        false
    }).await.unwrap_or(false);

    let mut builder = ContextBuilder::auth(&state)
        .page(PageType::AuthReset, "Reset Password");

    if valid {
        builder = builder.set("token", serde_json::json!(query.token));
    } else {
        builder = builder.set("error", serde_json::json!("error_reset_link_invalid"));
    }

    let data = builder.build();
    let data = state.hook_runner.run_before_render(data);

    match state.render("auth/reset_password", &data) {
        Ok(html) => Html(html),
        Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            },
    }
}

/// POST /admin/reset-password — validate token, update password, redirect to login.
pub async fn reset_password_action(
    State(state): State<AdminState>,
    Form(form): Form<ResetPasswordForm>,
) -> axum::response::Response {
    if form.password != form.password_confirm {
        let data = ContextBuilder::auth(&state)
            .page(PageType::AuthReset, "Reset Password")
            .set("token", serde_json::json!(form.token))
            .set("error", serde_json::json!("error_passwords_no_match"))
            .build();
        return match state.render("auth/reset_password", &data) {
            Ok(html) => Html(html).into_response(),
            Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            }.into_response(),
        };
    }

    if let Err(e) = state.config.auth.password_policy.validate(&form.password) {
        let data = ContextBuilder::auth(&state)
            .page(PageType::AuthReset, "Reset Password")
            .set("token", serde_json::json!(form.token))
            .set("error", serde_json::json!(e.to_string()))
            .build();
        return match state.render("auth/reset_password", &data) {
            Ok(html) => Html(html).into_response(),
            Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            }.into_response(),
        };
    }

    let pool = state.pool.clone();
    let registry = state.registry.clone();
    let token = form.token.clone();
    let password = form.password.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;

        // Search all auth collections for the token
        for def in registry.collections.values() {
            if !def.is_auth_collection() { continue; }
            if let Some((user, exp)) = query::find_by_reset_token(&conn, &def.slug, def, &token)? {
                if chrono::Utc::now().timestamp() >= exp {
                    query::clear_reset_token(&conn, &def.slug, &user.id)?;
                    return Err(anyhow::anyhow!("expired"));
                }
                // Update password and clear token
                query::update_password(&conn, &def.slug, &user.id, &password)?;
                query::clear_reset_token(&conn, &def.slug, &user.id)?;
                return Ok(());
            }
        }

        Err(anyhow::anyhow!("invalid_token"))
    }).await;

    match result {
        Ok(Ok(())) => {
            Redirect::to("/admin/login?success=success_password_reset").into_response()
        }
        Ok(Err(e)) => {
            let msg = if e.to_string().contains("expired") {
                "error_reset_link_expired"
            } else {
                "error_reset_link_invalid"
            };
            let data = ContextBuilder::auth(&state)
                .page(PageType::AuthReset, "Reset Password")
                .set("error", serde_json::json!(msg))
                .build();
            match state.render("auth/reset_password", &data) {
                Ok(html) => Html(html).into_response(),
                Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            }.into_response(),
            }
        }
        Err(e) => {
            tracing::error!("Reset password task error: {}", e);
            let data = ContextBuilder::auth(&state)
                .page(PageType::AuthReset, "Reset Password")
                .set("error", serde_json::json!("error_internal"))
                .build();
            match state.render("auth/reset_password", &data) {
                Ok(html) => Html(html).into_response(),
                Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            }.into_response(),
            }
        }
    }
}

// ── Email verification ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct VerifyEmailQuery {
    pub token: String,
}

/// GET /admin/verify-email?token=xxx — validate token, mark verified, redirect.
pub async fn verify_email(
    State(state): State<AdminState>,
    Query(query): Query<VerifyEmailQuery>,
) -> axum::response::Response {
    let pool = state.pool.clone();
    let registry = state.registry.clone();
    let token = query.token;

    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;

        for def in registry.collections.values() {
            if !def.is_auth_collection() { continue; }
            if !def.auth.as_ref().is_some_and(|a| a.verify_email) { continue; }
            if let Some((user, exp)) = query::find_by_verification_token(&conn, &def.slug, def, &token)? {
                if chrono::Utc::now().timestamp() >= exp {
                    // Token expired — don't verify
                    return Ok(false);
                }
                query::mark_verified(&conn, &def.slug, &user.id)?;
                return Ok(true);
            }
        }

        Ok::<_, anyhow::Error>(false)
    }).await;

    match result {
        Ok(Ok(true)) => {
            Redirect::to("/admin/login?success=success_email_verified").into_response()
        }
        _ => {
            Redirect::to("/admin/login").into_response()
        }
    }
}

// ── Locale preference ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct LocaleForm {
    pub locale: String,
}

/// POST /admin/api/locale — save user's preferred admin UI locale.
pub async fn save_locale(
    State(state): State<AdminState>,
    Extension(auth_user): Extension<crate::core::auth::AuthUser>,
    Form(form): Form<LocaleForm>,
) -> impl IntoResponse {
    // Validate locale is available
    let available = state.translations.available_locales();
    if !available.contains(&form.locale.as_str()) {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let pool = state.pool.clone();
    let user_id = auth_user.claims.sub.clone();
    let locale = form.locale.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let existing = query::get_user_settings(&conn, &user_id)?;
        let mut settings: serde_json::Value = existing
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_else(|| serde_json::json!({}));

        settings["ui_locale"] = serde_json::json!(locale);

        let json_str = serde_json::to_string(&settings)?;
        query::set_user_settings(&conn, &user_id, &json_str)?;
        Ok::<_, anyhow::Error>(())
    }).await;

    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

// ── Cookie helpers ────────────────────────────────────────────────────────

/// Build `Set-Cookie` header values for the session.
/// Returns two cookies: the HttpOnly JWT cookie and a JS-readable expiry cookie.
/// `exp` is the absolute Unix timestamp when the session expires.
fn session_cookies(token: &str, expiry: u64, exp: u64, dev_mode: bool) -> Vec<String> {
    let secure = if dev_mode { "" } else { "; Secure" };
    vec![
        format!(
            "crap_session={}; HttpOnly; Path=/; SameSite=Lax; Max-Age={}{}",
            token, expiry, secure,
        ),
        format!(
            "crap_session_exp={}; Path=/; SameSite=Lax; Max-Age={}{}",
            exp, expiry, secure,
        ),
    ]
}

/// Build `Set-Cookie` header values that clear both session cookies.
fn clear_session_cookies(dev_mode: bool) -> Vec<String> {
    let secure = if dev_mode { "" } else { "; Secure" };
    vec![
        format!("crap_session=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0{}", secure),
        format!("crap_session_exp=; Path=/; SameSite=Lax; Max-Age=0{}", secure),
    ]
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn login_error(state: &AdminState, error: &str, email: &str) -> axum::response::Response {
    let auth_collections = get_auth_collections(state);
    let all_disable_local = all_disable_local(state);
    let show_forgot_password = show_forgot_password(state);

    let data = ContextBuilder::auth(state)
        .page(PageType::AuthLogin, "Login")
        .set("error", serde_json::json!(error))
        .set("email", serde_json::json!(email))
        .set("collections", serde_json::json!(auth_collections))
        .set("show_collection_picker", serde_json::json!(auth_collections.len() > 1))
        .set("disable_local", serde_json::json!(all_disable_local))
        .set("show_forgot_password", serde_json::json!(show_forgot_password))
        .build();

    match state.render("auth/login", &data) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
                tracing::error!("Template render error: {}", e);
                Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
            }.into_response(),
    }
}

/// Check if all auth collections have disable_local = true.
fn all_disable_local(state: &AdminState) -> bool {
    let auth_collections: Vec<_> = state.registry.collections.values()
        .filter(|def| def.is_auth_collection())
        .collect();
    if auth_collections.is_empty() {
        return false;
    }
    auth_collections.iter().all(|def| {
        def.auth.as_ref().map(|a| a.disable_local).unwrap_or(false)
    })
}

/// Check if "forgot password?" link should show on login page.
/// Shows when: email is configured AND at least one auth collection has forgot_password enabled.
fn show_forgot_password(state: &AdminState) -> bool {
    if !email::is_configured(&state.config.email) {
        return false;
    }
    state.registry.collections.values()
        .filter(|def| def.is_auth_collection())
        .any(|def| def.auth.as_ref().is_some_and(|a| a.forgot_password))
}

fn get_auth_collections(state: &AdminState) -> Vec<serde_json::Value> {
    let mut collections: Vec<_> = state.registry.collections.values()
        .filter(|def| def.is_auth_collection())
        .map(|def| serde_json::json!({
            "slug": def.slug,
            "display_name": def.display_name(),
        }))
        .collect();
    collections.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));
    collections
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_cookies_dev_mode() {
        let cookies = session_cookies("tok123", 7200, 1700000000, true);
        assert_eq!(cookies.len(), 2);
        // JWT cookie
        assert!(cookies[0].contains("crap_session=tok123"));
        assert!(cookies[0].contains("HttpOnly"));
        assert!(cookies[0].contains("Max-Age=7200"));
        assert!(!cookies[0].contains("Secure"), "dev mode should not set Secure");
        // Exp cookie — JS-readable, no HttpOnly
        assert!(cookies[1].contains("crap_session_exp=1700000000"));
        assert!(!cookies[1].contains("HttpOnly"));
        assert!(cookies[1].contains("Max-Age=7200"));
        assert!(!cookies[1].contains("Secure"));
    }

    #[test]
    fn session_cookies_production_mode() {
        let cookies = session_cookies("tok456", 3600, 1700003600, false);
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0].contains("crap_session=tok456"));
        assert!(cookies[0].contains("Max-Age=3600"));
        assert!(cookies[0].contains("; Secure"), "production should set Secure");
        assert!(cookies[1].contains("crap_session_exp=1700003600"));
        assert!(!cookies[1].contains("HttpOnly"));
        assert!(cookies[1].contains("; Secure"));
    }

    #[test]
    fn clear_session_cookies_dev_mode() {
        let cookies = clear_session_cookies(true);
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0].contains("crap_session=;"));
        assert!(cookies[0].contains("Max-Age=0"));
        assert!(!cookies[0].contains("Secure"));
        assert!(cookies[1].contains("crap_session_exp=;"));
        assert!(cookies[1].contains("Max-Age=0"));
        assert!(!cookies[1].contains("HttpOnly"));
    }

    #[test]
    fn clear_session_cookies_production_mode() {
        let cookies = clear_session_cookies(false);
        assert_eq!(cookies.len(), 2);
        assert!(cookies[0].contains("crap_session=;"));
        assert!(cookies[0].contains("Max-Age=0"));
        assert!(cookies[0].contains("; Secure"));
        assert!(cookies[1].contains("crap_session_exp=;"));
        assert!(cookies[1].contains("; Secure"));
    }
}
