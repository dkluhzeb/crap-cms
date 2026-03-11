use axum::{
    extract::{Form, State},
    response::{IntoResponse, Redirect},
};

use crate::admin::AdminState;
use crate::core::auth;
use crate::core::auth::ClaimsBuilder;
use crate::db::query;
use super::{LoginForm, login_error, session_cookies};

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
