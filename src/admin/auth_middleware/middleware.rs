//! Admin auth middleware: extract credentials, run the unified
//! [`service::auth::evaluate`] evaluator + the admin UI-locale
//! lookup in a single `spawn_blocking`, route the outcome.

use std::collections::HashMap;

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode, header::COOKIE},
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use serde_json::Value;
use tokio::task::spawn_blocking;

use crate::admin::{
    AdminState,
    auth_middleware::{gate::check_admin_gate, pages::auth_required_response},
    handlers::{
        auth::{SESSION_COOKIE, append_cookies, clear_session_cookies, session_same_site},
        shared::paths,
    },
    server::extract_cookie,
};
use crate::core::{AuthUser, auth::Claims, collection::Surface};
use crate::db::{BoxedConnection, query};
use crate::service::{
    self,
    auth::{AuthFailure, AuthRequest, EvaluateDeps, Resolution},
};

/// Snapshot a `HeaderMap` into a plain `HashMap` for the evaluator
/// and downstream Lua strategy hooks. Keys retain whatever casing
/// the transport delivered (axum normalizes to lowercase already,
/// but we don't depend on it); case-insensitive matching is the
/// evaluator's job in `activation_matches`. Preserving the
/// original casing keeps the `ctx.headers` contract stable for
/// existing Lua hooks that may use mixed-case lookups.
fn headers_to_map(headers: &axum::http::HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|v| (name.as_str().to_string(), v.to_string()))
        })
        .collect()
}

/// Build a login redirect response (HTMX-aware: uses HX-Redirect
/// instead of 302).
#[cfg(not(tarpaulin_include))]
fn login_redirect(request: &Request<Body>) -> Response {
    let is_htmx = request.headers().get("HX-Request").is_some();

    if is_htmx {
        Response::builder()
            .status(StatusCode::OK)
            .header("HX-Redirect", paths::LOGIN)
            .body(Body::empty())
            .expect("static response builder")
    } else {
        Redirect::to(paths::LOGIN).into_response()
    }
}

/// Login redirect that also clears the session cookie. Used when
/// the evaluator returns `Resolution::Invalid` for a cookie-bound
/// JWT — without clearing, the browser keeps sending the stale
/// cookie and gets caught in an infinite redirect loop.
#[cfg(not(tarpaulin_include))]
fn login_redirect_clearing_cookie(state: &AdminState, request: &Request<Body>) -> Response {
    let mut response = login_redirect(request);
    let cookies = clear_session_cookies(state.config.admin.dev_mode, session_same_site(state));
    append_cookies(&mut response, &cookies);
    response
}

/// Insert authenticated claims (and optionally user) into request
/// extensions, checking the admin access gate first. Returns a
/// gate-denied response if blocked.
#[cfg(not(tarpaulin_include))]
async fn apply_auth_to_request(
    state: &AdminState,
    request: &mut Request<Body>,
    claims: Claims,
    auth_user: AuthUser,
) -> Option<Response> {
    if let Some(response) = check_admin_gate(state, &auth_user).await {
        return Some(response);
    }

    request.extensions_mut().insert(auth_user);
    request.extensions_mut().insert(claims);
    None
}

/// Bundled result of the single spawn-blocking call that drives
/// admin auth: the evaluator's [`Resolution`] plus the user's UI
/// locale preference looked up on the same connection (only when
/// authenticated).
struct AdminAuthOutcome {
    resolution: Resolution,
    ui_locale: Option<String>,
}

/// Load the admin-UI locale preference from `_crap_user_settings`
/// for the authenticated user. Falls back to `None` on any error —
/// caller substitutes the configured default locale. Uses the
/// already-acquired evaluator connection so we don't double-tap
/// the pool per request.
#[cfg(not(tarpaulin_include))]
fn load_ui_locale(conn: &BoxedConnection, user: &AuthUser) -> Option<String> {
    let settings_json = query::get_user_settings(conn, &user.claims.sub).ok()??;
    let value: Value = serde_json::from_str(&settings_json).ok()?;
    value
        .get("ui_locale")
        .and_then(|l| l.as_str())
        .map(str::to_string)
}

/// Auth middleware — resolves the request to a principal via the
/// unified evaluator, then enforces the two admin access gates:
///
/// 1. If no auth collections exist and `require_auth` is true,
///    shows the "setup required" page.
/// 2. If `admin.access` is configured, runs the Lua function
///    after a principal is in hand and refuses with a 403 page
///    on Deny.
#[cfg(not(tarpaulin_include))]
pub(in crate::admin) async fn auth_middleware(
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
    let session_token = extract_cookie(cookie_header, SESSION_COOKIE).map(str::to_string);

    let bearer_token = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let headers_map = headers_to_map(request.headers());

    let pool = state.pool.clone();
    let registry = state.registry.clone();
    let token_provider = state.token_provider.clone();
    let hook_runner = state.hook_runner.clone();

    let resolution = spawn_blocking(move || {
        let Ok(conn) = pool.get() else {
            // Pool exhaustion is a server-side problem, not an
            // auth problem — but the evaluator's contract returns
            // either Authenticated / Anonymous / Invalid.
            // Reporting it as a Lookup-class Invalid keeps the
            // cookie path coherent (the user retries; we don't
            // loop with cookie clear).
            return AdminAuthOutcome {
                resolution: Resolution::Invalid(AuthFailure::Lookup),
                ui_locale: None,
            };
        };
        let resolution = service::auth::evaluate(
            &AuthRequest {
                surface: Surface::Admin,
                bearer_token: bearer_token.as_deref(),
                session_cookie_token: session_token.as_deref(),
                headers: &headers_map,
            },
            &EvaluateDeps {
                registry: &registry,
                token_provider: token_provider.as_ref(),
                hook_runner: &hook_runner,
                conn: &conn,
            },
        );

        // While we still hold the connection, fetch the admin UI
        // locale preference for an authenticated user. Skips the
        // lookup entirely for Anonymous / Invalid — they won't
        // reach a logged-in page.
        let ui_locale = match &resolution {
            Resolution::Authenticated(auth) => load_ui_locale(&conn, &auth.user),
            _ => None,
        };

        AdminAuthOutcome {
            resolution,
            ui_locale,
        }
    })
    .await;

    let Ok(outcome) = resolution else {
        return login_redirect(&request);
    };

    let mut auth_user = match outcome.resolution {
        Resolution::Authenticated(auth) => auth.user,
        // Anonymous (no creds presented) and Invalid(Lookup) (a
        // transient pool/DB failure) both keep whatever cookie the
        // browser sent — there's no proven-dead token to clear,
        // and the user's next request might succeed.
        Resolution::Anonymous | Resolution::Invalid(AuthFailure::Lookup) => {
            return login_redirect(&request);
        }
        // Every other Invalid variant is a *definite* failure
        // (bad signature, stale session, locked, user gone,
        // unknown collection, issuer no longer accepts). Clear
        // the cookie so the browser stops sending the dead JWT
        // and the login page doesn't redirect-loop.
        Resolution::Invalid(_) => {
            return login_redirect_clearing_cookie(&state, &request);
        }
    };

    auth_user.ui_locale = outcome
        .ui_locale
        .unwrap_or_else(|| state.config.locale.default_locale.clone());

    let claims = auth_user.claims.clone();
    if let Some(response) = apply_auth_to_request(&state, &mut request, claims, auth_user).await {
        return response;
    }
    next.run(request).await
}
