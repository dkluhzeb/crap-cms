//! Login/logout handlers for the admin UI.

use axum::{
    extract::{Form, State},
    response::{Html, IntoResponse, Redirect},
};
use serde::Deserialize;

use crate::admin::AdminState;
use crate::core::auth;
use crate::db::query;

/// Form data submitted by the login page.
#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub collection: String,
    pub email: String,
    pub password: String,
}

/// GET /admin/login — render the login page.
pub async fn login_page(State(state): State<AdminState>) -> Html<String> {
    let auth_collections = get_auth_collections(&state);
    let all_disable_local = all_disable_local(&state);

    let data = serde_json::json!({
        "title": "Login",
        "collections": auth_collections,
        "show_collection_picker": auth_collections.len() > 1,
        "disable_local": all_disable_local,
    });

    match state.render("auth/login", &data) {
        Ok(html) => Html(html),
        Err(e) => Html(format!("<h1>Error</h1><pre>{}</pre>", e)),
    }
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

        Ok::<_, anyhow::Error>(Some(user))
    }).await;

    let user = match result {
        Ok(Ok(Some(user))) => user,
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

fn login_error(state: &AdminState, error: &str, email: &str) -> axum::response::Response {
    let auth_collections = get_auth_collections(state);
    let all_disable_local = all_disable_local(state);

    let data = serde_json::json!({
        "title": "Login",
        "error": error,
        "email": email,
        "collections": auth_collections,
        "show_collection_picker": auth_collections.len() > 1,
        "disable_local": all_disable_local,
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
