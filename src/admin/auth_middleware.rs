//! Auth middleware and helpers — JWT validation, custom strategies, admin access gates.

use std::collections::HashMap;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
};
use chrono::Utc;
use serde_json::Value;
use tracing::{debug, error, warn};

use crate::{
    admin::{AdminState, context::ContextBuilder, server::extract_cookie},
    config::LocaleConfig,
    core::{
        AuthUser, Registry, Slug,
        auth::{self, ClaimsBuilder},
        collection::Auth as CollectionAuth,
    },
    db::{DbPool, query},
    hooks::HookRunner,
    service::{self, ServiceContext},
};

use axum::extract::State;
use axum::http::header::COOKIE;

/// Validate JWT from `crap_session` cookie and optionally load the full user document.
#[cfg(not(tarpaulin_include))]
fn validate_jwt_and_load_user(
    state: &AdminState,
    cookie_header: &str,
) -> Option<(auth::Claims, Option<AuthUser>)> {
    let token = extract_cookie(cookie_header, "crap_session")?;

    let claims = match auth::validate_token(token, state.jwt_secret.as_ref()) {
        Ok(c) => c,
        Err(e) => {
            debug!("JWT validation failed: {}", e);
            return None;
        }
    };

    let auth_user = load_auth_user(&state.pool, &state.registry, &claims, &state.config.locale);

    // Reject locked users even if their JWT is still valid
    if let Some(ref au) = auth_user {
        let locked = au
            .user_doc
            .fields
            .get("_locked")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            != 0;

        if locked {
            return None;
        }
    }

    Some((claims, auth_user))
}

/// Evaluate custom auth strategies in a blocking context (Lua + DB access).
/// Tries each strategy across all auth collections until one succeeds.
#[cfg(not(tarpaulin_include))]
fn try_strategy_auth(
    auth_defs: &[(Slug, CollectionAuth)],
    headers: &HashMap<String, String>,
    pool: &DbPool,
    hook_runner: &HookRunner,
) -> Option<auth::Claims> {
    let mut conn = pool.get().ok()?;
    let tx = conn.transaction().ok()?;
    let mut result = None;

    for (slug, auth_config) in auth_defs {
        if result.is_some() {
            break;
        }

        for strategy in &auth_config.strategies {
            match hook_runner.run_auth_strategy(&strategy.authenticate, slug, headers, &tx) {
                Ok(Some(user)) => {
                    let user_email = user
                        .fields
                        .get("email")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let expiry = auth_config.token_expiry;

                    let now = Utc::now().timestamp().max(0) as u64;

                    let claims = match ClaimsBuilder::new(user.id.clone(), slug.clone())
                        .email(user_email)
                        .exp(now.saturating_add(expiry))
                        .auth_time(now)
                        .build()
                    {
                        Ok(c) => c,
                        Err(e) => {
                            error!("Claims build error: {}", e);
                            continue;
                        }
                    };

                    result = Some(claims);
                    break;
                }
                Ok(None) => continue,
                Err(e) => {
                    warn!(
                        "Auth strategy '{}' error for {}: {}",
                        strategy.name, slug, e
                    );
                    continue;
                }
            }
        }
    }

    if let Err(e) = tx.commit() {
        warn!("tx commit failed: {e}");
    }

    result
}

/// Build a login redirect response (HTMX-aware: uses HX-Redirect instead of 302).
#[cfg(not(tarpaulin_include))]
fn login_redirect(request: &Request<Body>) -> Response {
    let is_htmx = request.headers().get("HX-Request").is_some();

    if is_htmx {
        Response::builder()
            .status(StatusCode::OK)
            .header("HX-Redirect", "/admin/login")
            .body(Body::empty())
            .expect("static response builder")
    } else {
        Redirect::to("/admin/login").into_response()
    }
}

/// Insert authenticated claims (and optionally user) into request extensions,
/// checking the admin access gate first. Returns a gate-denied response if blocked.
#[cfg(not(tarpaulin_include))]
async fn apply_auth_to_request(
    state: &AdminState,
    request: &mut Request<Body>,
    claims: auth::Claims,
    auth_user: Option<AuthUser>,
) -> Option<Response> {
    if let Some(user) = auth_user {
        if let Some(response) = check_admin_gate(state, &user).await {
            return Some(response);
        }

        request.extensions_mut().insert(user);
    }

    request.extensions_mut().insert(claims);
    None
}

/// Auth middleware — extracts JWT from `crap_session` cookie, validates it,
/// and stores `Claims` in request extensions. If JWT is invalid/missing,
/// tries custom auth strategies before redirecting to login.
///
/// Also enforces two admin access gates:
/// 1. If no auth collections exist and `require_auth` is true, shows "setup required" page.
/// 2. If `admin.access` is configured, checks the Lua function after authentication.
#[cfg(not(tarpaulin_include))]
pub(super) async fn auth_middleware(
    State(state): State<AdminState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    // Gate 1: No auth collections but require_auth is on → setup required
    if !state.has_auth && state.config.admin.require_auth {
        return auth_required_response(&state);
    }

    let cookie_header = request
        .headers()
        .get(COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Fast path: valid JWT cookie
    if let Some((claims, auth_user)) = validate_jwt_and_load_user(&state, cookie_header) {
        if let Some(response) = apply_auth_to_request(&state, &mut request, claims, auth_user).await
        {
            return response;
        }

        return next.run(request).await;
    }

    // Collect custom strategies from all auth collections
    let auth_defs: Vec<_> = state
        .registry
        .collections
        .values()
        .filter(|d| d.is_auth_collection())
        .filter(|d| {
            d.auth
                .as_ref()
                .map(|a| !a.strategies.is_empty())
                .unwrap_or(false)
        })
        .map(|d| (d.slug.clone(), d.auth.clone().expect("guarded by filter")))
        .collect();

    if !auth_defs.is_empty() {
        let headers: HashMap<String, String> = request
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.as_str().to_string(), v.to_string()))
            })
            .collect();

        let pool = state.pool.clone();
        let hook_runner = state.hook_runner.clone();

        let strategy_result = tokio::task::spawn_blocking(move || {
            try_strategy_auth(&auth_defs, &headers, &pool, &hook_runner)
        })
        .await;

        if let Ok(Some(claims)) = strategy_result {
            let auth_user =
                load_auth_user(&state.pool, &state.registry, &claims, &state.config.locale);

            if let Some(response) =
                apply_auth_to_request(&state, &mut request, claims, auth_user).await
            {
                return response;
            }

            return next.run(request).await;
        }
    }

    login_redirect(&request)
}

/// Gate 2: Check `admin.access` Lua function. Returns a 403 response if the user
/// is denied, or None if access is allowed (or no access function is configured).
#[cfg(not(tarpaulin_include))]
async fn check_admin_gate(state: &AdminState, auth_user: &AuthUser) -> Option<Response> {
    check_admin_gate_for_doc(state, &auth_user.user_doc).await
}

/// Check `admin.access` against a user document. Used by both the auth middleware
/// and the login handler to enforce the gate before issuing a session.
#[cfg(not(tarpaulin_include))]
pub(crate) async fn check_admin_gate_for_doc(
    state: &AdminState,
    user_doc: &crate::core::Document,
) -> Option<Response> {
    let access_ref = state.config.admin.access.as_deref()?;
    let pool = state.pool.clone();
    let hook_runner = state.hook_runner.clone();
    let user_doc = user_doc.clone();
    let access_ref = access_ref.to_string();

    let result = tokio::task::spawn_blocking(move || {
        let conn = pool.get().ok()?;
        Some(hook_runner.check_access(Some(&access_ref), Some(&user_doc), None, None, &conn))
    })
    .await;

    match result {
        Ok(Some(Ok(query::AccessResult::Denied))) => Some(admin_denied_response(state)),
        Ok(Some(Err(e))) => {
            error!("admin.access check failed: {}", e);
            Some(admin_denied_response(state))
        }
        _ => None,
    }
}

/// Render the "setup required" page (no auth collection exists, require_auth is on).
fn auth_required_response(state: &AdminState) -> Response {
    let data = ContextBuilder::auth(state).build();

    match state.render("errors/auth_required", &data) {
        Ok(html) => (StatusCode::SERVICE_UNAVAILABLE, Html(html)).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "Setup required: no auth collection configured",
        )
            .into_response(),
    }
}

/// Render the "access denied" page (user authenticated but not authorized for admin).
fn admin_denied_response(state: &AdminState) -> Response {
    let data = ContextBuilder::auth(state).build();

    match state.render("errors/admin_denied", &data) {
        Ok(html) => (StatusCode::FORBIDDEN, Html(html)).into_response(),
        Err(_) => (StatusCode::FORBIDDEN, "Access denied").into_response(),
    }
}

/// Load the full user document for an authenticated user.
/// Returns None if the user can't be found (e.g., deleted since JWT was issued).
/// Also loads `ui_locale` from `_crap_user_settings`.
#[cfg(not(tarpaulin_include))]
pub(crate) fn load_auth_user(
    pool: &DbPool,
    registry: &Registry,
    claims: &auth::Claims,
    locale_config: &LocaleConfig,
) -> Option<AuthUser> {
    let def = registry.get_collection(&claims.collection)?.clone();
    let locale_ctx = query::LocaleContext::from_locale_string(None, locale_config).unwrap_or(None);

    let conn = pool.get().ok()?;

    let doc = query::find_by_id(
        &conn,
        &claims.collection,
        &def,
        &claims.sub,
        locale_ctx.as_ref(),
    )
    .ok()??;

    // Reject tokens with stale session version (password was changed).
    let ctx = ServiceContext::slug_only(&claims.collection)
        .conn(&conn)
        .build();
    let db_session_version = service::auth::get_session_version(&ctx, &claims.sub).ok()?;

    if claims.session_version != db_session_version {
        return None;
    }

    // Load ui_locale from _crap_user_settings
    let ui_locale = query::get_user_settings(&conn, &claims.sub)
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| {
            v.get("ui_locale")
                .and_then(|l| l.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| locale_config.default_locale.clone());

    let mut auth = AuthUser::new(claims.clone(), doc);
    auth.ui_locale = ui_locale;

    Some(auth)
}
