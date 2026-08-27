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
    auth_middleware::{
        gate::{check_admin_gate, check_collection_admin_gate},
        pages::auth_required_response,
    },
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

    // Per-collection `access.admin` gate for any admin route scoped to a
    // collection — blocks direct-URL access to a collection's admin pages, not
    // just its nav entry. No-op for non-collection paths or collections without
    // an `access.admin` rule.
    if let Some(slug) = collection_slug_from_path(request.uri().path())
        && let Some(response) = check_collection_admin_gate(state, slug, &auth_user.user_doc).await
    {
        return Some(response);
    }

    request.extensions_mut().insert(auth_user);
    request.extensions_mut().insert(claims);
    None
}

/// Extract the collection/global slug from an admin request path for the
/// `access.admin` gate. Covers collection admin pages + their collection-scoped
/// APIs and global admin pages; returns `None` for the collections index, the
/// dashboard/custom-page routes, and anything else.
fn collection_slug_from_path(path: &str) -> Option<&str> {
    let rest = path
        .strip_prefix("/admin/collections/")
        .or_else(|| path.strip_prefix("/admin/globals/"))
        .or_else(|| path.strip_prefix("/admin/api/search/"))
        .or_else(|| path.strip_prefix("/admin/api/user-settings/"))?;
    let slug = rest.split('/').next()?;
    (!slug.is_empty()).then_some(slug)
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

    let pool = state.infra.pool.clone();
    let registry = state.infra.registry.clone();
    let token_provider = state.infra.token_provider.clone();
    let hook_runner = state.infra.hook_runner.clone();

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

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::*;

    #[test]
    fn maps_header_names_to_string_values() {
        let mut h = HeaderMap::new();
        h.insert("content-type", "application/json".parse().unwrap());
        h.insert("x-custom", "abc".parse().unwrap());

        let m = headers_to_map(&h);
        assert_eq!(
            m.get("content-type").map(String::as_str),
            Some("application/json")
        );
        assert_eq!(m.get("x-custom").map(String::as_str), Some("abc"));
    }

    #[test]
    fn skips_non_utf8_header_values() {
        let mut h = HeaderMap::new();
        h.insert("x-bin", HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap());
        h.insert("x-ok", "v".parse().unwrap());

        let m = headers_to_map(&h);
        assert!(!m.contains_key("x-bin")); // non-UTF8 value dropped
        assert_eq!(m.get("x-ok").map(String::as_str), Some("v"));
    }

    #[test]
    fn extracts_collection_slug_for_admin_gate() {
        // Collection admin pages + collection-scoped APIs → the slug.
        assert_eq!(
            collection_slug_from_path("/admin/collections/posts"),
            Some("posts")
        );
        assert_eq!(
            collection_slug_from_path("/admin/collections/posts/123/versions"),
            Some("posts")
        );
        assert_eq!(
            collection_slug_from_path("/admin/collections/posts/create"),
            Some("posts")
        );
        assert_eq!(
            collection_slug_from_path("/admin/api/search/posts"),
            Some("posts")
        );
        assert_eq!(
            collection_slug_from_path("/admin/api/user-settings/posts"),
            Some("posts")
        );

        // Globals are gated too (they have admin pages).
        assert_eq!(
            collection_slug_from_path("/admin/globals/settings"),
            Some("settings")
        );

        // Not scoped to a collection/global → no slug (gate is a no-op).
        assert_eq!(collection_slug_from_path("/admin/collections"), None);
        assert_eq!(collection_slug_from_path("/admin"), None);
        assert_eq!(collection_slug_from_path("/admin/collections/"), None);
    }
}
