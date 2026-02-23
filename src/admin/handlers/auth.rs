//! Login/logout/forgot-password/reset-password/verify-email handlers for the admin UI.

use axum::{
    extract::{Form, Query, State},
    response::{Html, IntoResponse, Redirect},
};
use serde::Deserialize;

use crate::admin::AdminState;
use crate::core::auth;
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

    let data = serde_json::json!({
        "title": "Login",
        "collections": auth_collections,
        "show_collection_picker": auth_collections.len() > 1,
        "disable_local": all_disable_local,
        "show_forgot_password": show_forgot_password,
        "success": query.success.as_deref(),
    });

    match state.render("auth/login", &data) {
        Ok(html) => Html(html),
        Err(e) => Html(format!("<h1>Error</h1><pre>{}</pre>", e)),
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
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(_) => return login_error(&state, "Internal error", &form.email),
        };
        reg.get_collection(&form.collection).cloned()
    };

    let def = match def {
        Some(d) if d.is_auth_collection() => d,
        _ => return login_error(&state, "Invalid collection", &form.email),
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
            None => return Ok(None),
        };

        // Verify password
        let hash = query::get_password_hash(&conn, &slug, &user.id)?;
        let hash = match hash {
            Some(h) => h,
            None => return Ok(None),
        };

        if !auth::verify_password(&password, &hash)? {
            return Ok(None);
        }

        // Check email verification if enabled
        if verify_email {
            let verified = query::is_verified(&conn, &slug, &user.id)?;
            if !verified {
                return Ok(Some(Err("Please verify your email before logging in".to_string())));
            }
        }

        Ok::<_, anyhow::Error>(Some(Ok(user)))
    }).await;

    let user = match result {
        Ok(Ok(Some(Ok(user)))) => user,
        Ok(Ok(Some(Err(msg)))) => return login_error(&state, &msg, &form.email),
        Ok(Ok(None)) => return login_error(&state, "Invalid email or password", &form.email),
        Ok(Err(e)) => {
            tracing::error!("Login error: {}", e);
            return login_error(&state, "Internal error", &form.email);
        }
        Err(e) => {
            tracing::error!("Login task error: {}", e);
            return login_error(&state, "Internal error", &form.email);
        }
    };

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
    let claims = auth::Claims {
        sub: user.id.clone(),
        collection: form.collection.clone(),
        email: user_email,
        exp: (chrono::Utc::now().timestamp() as u64) + expiry,
    };

    let token = match auth::create_token(&claims, &state.jwt_secret) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Token creation error: {}", e);
            return login_error(&state, "Internal error", &form.email);
        }
    };

    // Set cookie and redirect
    let cookie = format!(
        "crap_session={}; HttpOnly; Path=/; SameSite=Lax; Max-Age={}",
        token, expiry
    );

    let mut response = Redirect::to("/admin").into_response();
    response.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        cookie.parse().unwrap(),
    );
    response
}

/// POST /admin/logout — clear cookie, redirect to login.
pub async fn logout_action() -> axum::response::Response {
    let cookie = "crap_session=; HttpOnly; Path=/; SameSite=Lax; Max-Age=0";
    let mut response = Redirect::to("/admin/login").into_response();
    response.headers_mut().insert(
        axum::http::header::SET_COOKIE,
        cookie.parse().unwrap(),
    );
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

    let data = serde_json::json!({
        "title": "Forgot Password",
        "collections": auth_collections,
        "show_collection_picker": auth_collections.len() > 1,
    });

    match state.render("auth/forgot_password", &data) {
        Ok(html) => Html(html),
        Err(e) => Html(format!("<h1>Error</h1><pre>{}</pre>", e)),
    }
}

/// POST /admin/forgot-password — look up user, generate token, send email.
/// Always shows success (don't leak whether email exists).
pub async fn forgot_password_action(
    State(state): State<AdminState>,
    Form(form): Form<ForgotPasswordForm>,
) -> Html<String> {
    let auth_collections = get_auth_collections(&state);

    // Try to find user and send reset email in background
    let def = {
        let reg = match state.registry.read() {
            Ok(r) => r,
            Err(_) => {
                return render_forgot_success(&state, &auth_collections);
            }
        };
        reg.get_collection(&form.collection).cloned()
    };

    if let Some(def) = def {
        if def.is_auth_collection() && def.auth.as_ref().is_some_and(|a| a.forgot_password) {
            let pool = state.pool.clone();
            let slug = form.collection.clone();
            let user_email = form.email.clone();
            let def_owned = def;
            let email_config = state.config.email.clone();
            let admin_port = state.config.server.admin_port;
            let host = state.config.server.host.clone();

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
                let exp = chrono::Utc::now().timestamp() + 3600; // 1 hour

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
                    "expiry_minutes": 60,
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
    let data = serde_json::json!({
        "title": "Forgot Password",
        "success": true,
        "collections": auth_collections,
        "show_collection_picker": auth_collections.len() > 1,
    });

    match state.render("auth/forgot_password", &data) {
        Ok(html) => Html(html),
        Err(e) => Html(format!("<h1>Error</h1><pre>{}</pre>", e)),
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
        let reg = match registry.read() {
            Ok(r) => r,
            Err(_) => return false,
        };
        for def in reg.collections.values() {
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

    let data = if valid {
        serde_json::json!({
            "title": "Reset Password",
            "token": query.token,
        })
    } else {
        serde_json::json!({
            "title": "Reset Password",
            "error": "Invalid or expired reset link. Please request a new one.",
        })
    };

    match state.render("auth/reset_password", &data) {
        Ok(html) => Html(html),
        Err(e) => Html(format!("<h1>Error</h1><pre>{}</pre>", e)),
    }
}

/// POST /admin/reset-password — validate token, update password, redirect to login.
pub async fn reset_password_action(
    State(state): State<AdminState>,
    Form(form): Form<ResetPasswordForm>,
) -> axum::response::Response {
    if form.password != form.password_confirm {
        let data = serde_json::json!({
            "title": "Reset Password",
            "token": form.token,
            "error": "Passwords do not match",
        });
        return match state.render("auth/reset_password", &data) {
            Ok(html) => Html(html).into_response(),
            Err(e) => Html(format!("<h1>Error</h1><pre>{}</pre>", e)).into_response(),
        };
    }

    if form.password.len() < 6 {
        let data = serde_json::json!({
            "title": "Reset Password",
            "token": form.token,
            "error": "Password must be at least 6 characters",
        });
        return match state.render("auth/reset_password", &data) {
            Ok(html) => Html(html).into_response(),
            Err(e) => Html(format!("<h1>Error</h1><pre>{}</pre>", e)).into_response(),
        };
    }

    let pool = state.pool.clone();
    let registry = state.registry.clone();
    let token = form.token.clone();
    let password = form.password.clone();

    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let reg = registry.read()
            .map_err(|e| anyhow::anyhow!("Registry lock: {}", e))?;

        // Search all auth collections for the token
        for def in reg.collections.values() {
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
            Redirect::to("/admin/login?success=Password+reset+successfully").into_response()
        }
        Ok(Err(e)) => {
            let msg = if e.to_string().contains("expired") {
                "Reset link has expired. Please request a new one."
            } else {
                "Invalid or expired reset link."
            };
            let data = serde_json::json!({
                "title": "Reset Password",
                "error": msg,
            });
            match state.render("auth/reset_password", &data) {
                Ok(html) => Html(html).into_response(),
                Err(e) => Html(format!("<h1>Error</h1><pre>{}</pre>", e)).into_response(),
            }
        }
        Err(e) => {
            tracing::error!("Reset password task error: {}", e);
            let data = serde_json::json!({
                "title": "Reset Password",
                "error": "An internal error occurred",
            });
            match state.render("auth/reset_password", &data) {
                Ok(html) => Html(html).into_response(),
                Err(e) => Html(format!("<h1>Error</h1><pre>{}</pre>", e)).into_response(),
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
        let reg = registry.read()
            .map_err(|e| anyhow::anyhow!("Registry lock: {}", e))?;

        for def in reg.collections.values() {
            if !def.is_auth_collection() { continue; }
            if !def.auth.as_ref().is_some_and(|a| a.verify_email) { continue; }
            if let Some(user) = query::find_by_verification_token(&conn, &def.slug, def, &token)? {
                query::mark_verified(&conn, &def.slug, &user.id)?;
                return Ok(true);
            }
        }

        Ok::<_, anyhow::Error>(false)
    }).await;

    match result {
        Ok(Ok(true)) => {
            Redirect::to("/admin/login?success=Email+verified+successfully").into_response()
        }
        _ => {
            Redirect::to("/admin/login").into_response()
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn login_error(state: &AdminState, error: &str, email: &str) -> axum::response::Response {
    let auth_collections = get_auth_collections(state);
    let all_disable_local = all_disable_local(state);
    let show_forgot_password = show_forgot_password(state);

    let data = serde_json::json!({
        "title": "Login",
        "error": error,
        "email": email,
        "collections": auth_collections,
        "show_collection_picker": auth_collections.len() > 1,
        "disable_local": all_disable_local,
        "show_forgot_password": show_forgot_password,
    });

    match state.render("auth/login", &data) {
        Ok(html) => Html(html).into_response(),
        Err(e) => Html(format!("<h1>Error</h1><pre>{}</pre>", e)).into_response(),
    }
}

/// Check if all auth collections have disable_local = true.
fn all_disable_local(state: &AdminState) -> bool {
    let reg = match state.registry.read() {
        Ok(r) => r,
        Err(_) => return false,
    };
    let auth_collections: Vec<_> = reg.collections.values()
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
    let reg = match state.registry.read() {
        Ok(r) => r,
        Err(_) => return false,
    };
    reg.collections.values()
        .filter(|def| def.is_auth_collection())
        .any(|def| def.auth.as_ref().is_some_and(|a| a.forgot_password))
}

fn get_auth_collections(state: &AdminState) -> Vec<serde_json::Value> {
    let reg = match state.registry.read() {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut collections: Vec<_> = reg.collections.values()
        .filter(|def| def.is_auth_collection())
        .map(|def| serde_json::json!({
            "slug": def.slug,
            "display_name": def.display_name(),
        }))
        .collect();
    collections.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));
    collections
}
