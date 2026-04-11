//! Shared helper functions for auth handlers.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
};

use axum::{
    http::HeaderMap,
    response::{Html, IntoResponse, Redirect, Response},
};
use chrono::Utc;
use serde_json::{Value, json};
use tracing::error;

use crate::{
    admin::{
        AdminState,
        context::{ContextBuilder, PageType},
        handlers::auth::{append_cookies, session_cookies},
    },
    core::{Document, Registry, Slug, auth::ClaimsBuilder, email},
};

/// Extract client IP from the request.
/// When `trust_proxy` is true, uses the first entry in X-Forwarded-For (for reverse proxy setups).
/// When false, uses the TCP socket address — XFF is ignored to prevent spoofing.
/// The result is always a canonical IP string (parsed and re-serialized) to prevent
/// rate limiter bypasses via alternative IPv6 representations.
pub(in crate::admin::handlers) fn client_ip(
    headers: &HeaderMap,
    addr: &SocketAddr,
    trust_proxy: bool,
) -> String {
    if trust_proxy
        && let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(first) = xff.split(',').next().map(str::trim)
        && !first.is_empty()
    {
        // Parse and re-serialize to normalize IPv6 representations
        // (e.g., "2001:0db8::0001" → "2001:db8::1").
        // Unparseable XFF falls back to socket address — not the raw string,
        // which an attacker could vary per-request to bypass rate limiting.
        return first
            .parse::<IpAddr>()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|_| addr.ip().to_string());
    }

    addr.ip().to_string()
}

pub(in crate::admin::handlers) fn login_error(
    state: &AdminState,
    error: &str,
    email: &str,
) -> Response {
    let auth_collections = get_auth_collections(state);
    let all_disable_local = all_disable_local(state);
    let show_forgot_password = show_forgot_password(state);

    let data = ContextBuilder::auth(state)
        .page(PageType::AuthLogin, "Login")
        .set("error", json!(error))
        .set("email", json!(email))
        .set("collections", json!(auth_collections))
        .set("show_collection_picker", json!(auth_collections.len() > 1))
        .set("disable_local", json!(all_disable_local))
        .set("show_forgot_password", json!(show_forgot_password))
        .build();

    match state.render("auth/login", &data) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            error!("Template render error: {}", e);

            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
        .into_response(),
    }
}

/// Check if all auth collections have disable_local = true.
pub(in crate::admin::handlers) fn all_disable_local(state: &AdminState) -> bool {
    let auth_collections: Vec<_> = state
        .registry
        .collections
        .values()
        .filter(|def| def.is_auth_collection())
        .collect();

    if auth_collections.is_empty() {
        return false;
    }

    auth_collections
        .iter()
        .all(|def| def.auth.as_ref().map(|a| a.disable_local).unwrap_or(false))
}

/// Check if "forgot password?" link should show on login page.
pub(in crate::admin::handlers) fn show_forgot_password(state: &AdminState) -> bool {
    if !email::is_configured(&state.config.email) {
        return false;
    }

    state
        .registry
        .collections
        .values()
        .filter(|def| def.is_auth_collection())
        .any(|def| def.auth.as_ref().is_some_and(|a| a.forgot_password))
}

pub(in crate::admin::handlers) fn get_auth_collections(state: &AdminState) -> Vec<Value> {
    let mut collections: Vec<_> = state
        .registry
        .collections
        .values()
        .filter(|def| def.is_auth_collection())
        .map(|def| {
            json!({
                "slug": def.slug,
                "display_name": def.display_name(),
            })
        })
        .collect();

    collections.sort_by(|a, b| a["slug"].as_str().cmp(&b["slug"].as_str()));

    collections
}

pub(in crate::admin::handlers) fn render_forgot_success(
    state: &AdminState,
    auth_collections: &[Value],
) -> Html<String> {
    let data = ContextBuilder::auth(state)
        .page(PageType::AuthForgot, "Forgot Password")
        .set("success", json!(true))
        .set("collections", json!(auth_collections))
        .set("show_collection_picker", json!(auth_collections.len() > 1))
        .build();

    match state.render("auth/forgot_password", &data) {
        Ok(html) => Html(html),
        Err(e) => {
            error!("Template render error: {}", e);

            Html("<h1>Something went wrong</h1><p>Please try again.</p>".to_string())
        }
    }
}

/// Convert axum `HeaderMap` to a simple `HashMap<String, String>`.
pub(in crate::admin::handlers) fn headers_to_map(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_string())))
        .collect()
}

/// Find the slug of the first auth collection in the registry.
pub(in crate::admin::handlers) fn find_auth_collection(registry: &Registry) -> Option<String> {
    registry
        .collections
        .iter()
        .find(|(_, d)| d.is_auth_collection())
        .map(|(slug, _)| slug.to_string())
}

/// Extract the email field from a user document, defaulting to empty string.
pub(in crate::admin::handlers) fn extract_user_email(user: &Document) -> String {
    user.fields
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Result of creating a session token.
pub(in crate::admin::handlers) struct SessionToken {
    pub token: String,
    pub expiry: u64,
    pub exp: u64,
}

/// Build a JWT session token for a user, resolving expiry from collection config or global default.
pub(in crate::admin::handlers) fn create_session_token(
    state: &AdminState,
    user_id: String,
    collection: &str,
    email: String,
    session_version: u64,
) -> Result<SessionToken, String> {
    let expiry = state
        .registry
        .get_collection(collection)
        .and_then(|def| def.auth.as_ref().map(|a| a.token_expiry))
        .unwrap_or(state.config.auth.token_expiry);

    let claims = ClaimsBuilder::new(user_id, Slug::new(collection))
        .email(email)
        .exp((Utc::now().timestamp().max(0) as u64).saturating_add(expiry))
        .session_version(session_version)
        .build()
        .map_err(|e| format!("Claims build error: {}", e))?;

    let token = state
        .token_provider
        .create_token(&claims)
        .map_err(|e| format!("Token creation error: {}", e))?;

    Ok(SessionToken {
        token,
        expiry,
        exp: claims.exp,
    })
}

/// Build a redirect-to-/admin response with session cookies set.
pub(in crate::admin::handlers) fn session_redirect(
    session: &SessionToken,
    dev_mode: bool,
) -> Response {
    let cookies = session_cookies(&session.token, session.expiry, session.exp, dev_mode);
    let mut response = Redirect::to("/admin").into_response();

    append_cookies(&mut response, &cookies);

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_ip_trust_proxy_reads_xff() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "10.0.0.1, 192.168.1.1".parse().unwrap());
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        assert_eq!(client_ip(&headers, &addr, true), "10.0.0.1");
    }

    #[test]
    fn client_ip_no_trust_proxy_ignores_xff() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "10.0.0.1, 192.168.1.1".parse().unwrap());
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        assert_eq!(client_ip(&headers, &addr, false), "127.0.0.1");
    }

    #[test]
    fn client_ip_falls_back_to_addr() {
        let headers = HeaderMap::new();
        let addr: SocketAddr = "192.168.1.5:5678".parse().unwrap();
        assert_eq!(client_ip(&headers, &addr, true), "192.168.1.5");
    }

    #[test]
    fn client_ip_ignores_empty_xff() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "".parse().unwrap());
        let addr: SocketAddr = "10.0.0.2:80".parse().unwrap();
        assert_eq!(client_ip(&headers, &addr, true), "10.0.0.2");
    }

    #[test]
    fn client_ip_normalizes_ipv6_xff() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "2001:0db8:0000:0000:0000:0000:0000:0001".parse().unwrap(),
        );
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        // Must normalize to canonical form to prevent rate limiter bypass
        assert_eq!(client_ip(&headers, &addr, true), "2001:db8::1");
    }

    #[test]
    fn client_ip_handles_unparseable_xff() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "not-an-ip".parse().unwrap());
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        // Falls back to socket address when XFF is unparseable (prevents rate limiter bypass)
        assert_eq!(client_ip(&headers, &addr, true), "127.0.0.1");
    }
}
