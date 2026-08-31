//! Shared helper functions for auth handlers.

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
};

use axum::{
    http::{HeaderMap, header::COOKIE},
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use ipnet::IpNet;

use crate::admin::handlers::shared::HxNav;
use crate::core::collection::Auth;
use crate::{
    admin::{
        AdminState,
        context::{
            AuthBasePageContext, PageMeta, PageType,
            page::auth::{AuthCollection, ForgotPasswordPage, LoginPage, MfaPage},
        },
        handlers::{
            auth::{MFA_PENDING_COOKIE, append_cookies, session_cookies, session_same_site},
            shared::render_page,
        },
        server::extract_cookie,
    },
    config::ServerConfig,
    core::{Document, Registry, Slug, auth::ClaimsBuilder, email, rate_limit::LoginRateLimiter},
};

/// Build an ad-hoc rate limiter that reuses the global rate-limit backend under
/// a distinct keyspace `prefix`. Sharing the backend keeps cross-instance state
/// consistent on Redis-backed deployments (the same pattern the per-route rate
/// limits use), so a limiter that isn't worth its own `AdminState` field can
/// still be scoped correctly without wiping unrelated buckets.
pub(in crate::admin) fn scoped_limiter(
    state: &AdminState,
    prefix: &str,
    max_attempts: u32,
    window_seconds: u64,
) -> LoginRateLimiter {
    LoginRateLimiter::with_backend(
        state.login_limiter.backend(),
        prefix,
        max_attempts,
        window_seconds,
    )
}

/// Extract the client IP from the request, honoring `X-Forwarded-For`
/// only when the peer address is a configured trusted proxy.
///
/// Behavior:
/// - `trust_proxy = false` (default) → always return the TCP peer IP.
/// - `trust_proxy = true` → `trusted_proxies` is required (enforced at
///   startup, see `CrapConfig::validate_trusted_proxies`). XFF is honored
///   only when the direct peer IP matches an entry in `trusted_proxies`;
///   otherwise the peer IP is returned (spoof-resistant). `trusted_proxies`
///   accepts bare IPs, CIDR ranges, and the `"*"` wildcard for explicit
///   opt-in "trust any peer" deployments.
///
/// The returned IP string is always canonical (parsed and re-serialized)
/// so that alternative IPv6 representations can't be used to rotate
/// per-IP rate-limit buckets.
pub(in crate::admin::handlers) fn client_ip(
    headers: &HeaderMap,
    addr: &SocketAddr,
    server: &ServerConfig,
) -> String {
    if !server.trust_proxy {
        return addr.ip().to_string();
    }

    let peer_ip = addr.ip();

    // Require the direct peer IP to be in the configured allowlist before
    // honoring XFF. An empty allowlist is rejected at startup — reaching
    // here with one means no peer qualifies, so fall back to the socket
    // address. `ip_is_trusted` also treats the `"*"` wildcard as always-true.
    if !ip_is_trusted(peer_ip, &server.trusted_proxies) {
        return peer_ip.to_string();
    }

    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(first) = xff.split(',').next().map(str::trim)
        && !first.is_empty()
    {
        // Parse and re-serialize to normalize IPv6 representations
        // (e.g., "2001:0db8::0001" → "2001:db8::1").
        // Unparseable XFF falls back to socket address — not the raw string,
        // which an attacker could vary per-request to bypass rate limiting.
        return first
            .parse::<IpAddr>()
            .map_or_else(|_| peer_ip.to_string(), |ip| ip.to_string());
    }

    peer_ip.to_string()
}

/// Check whether an IP is allowed to set `X-Forwarded-For` per the
/// configured allowlist. Entries are parsed as bare IPs or CIDR ranges;
/// `"*"` is a wildcard that trusts any peer (operators opt in at
/// startup — see `CrapConfig::validate_trusted_proxies`). Malformed
/// entries are rejected at startup, so none should reach this code.
fn ip_is_trusted(ip: IpAddr, trusted: &[String]) -> bool {
    trusted.iter().any(|entry| {
        if entry == "*" {
            return true;
        }

        match entry.parse::<IpNet>() {
            Ok(net) => net.contains(&ip),
            Err(_) => entry.parse::<IpAddr>().is_ok_and(|a| a == ip),
        }
    })
}

pub(in crate::admin::handlers) fn login_error(
    state: &AdminState,
    error: &str,
    email: &str,
) -> Response {
    let auth_collections = get_auth_collections(state);
    let show_collection_picker = auth_collections.len() > 1;

    let ctx = LoginPage {
        base: AuthBasePageContext::for_state(state, PageMeta::new(PageType::AuthLogin, "Login")),
        error: Some(error.to_string()),
        email: Some(email.to_string()),
        collections: auth_collections,
        show_collection_picker,
        disable_local: all_disable_local(state),
        show_forgot_password: show_forgot_password(state),
        success: None,
    };

    render_page(state, HxNav::full(), "auth/login", &ctx)
}

/// Check if every auth collection has password-login turned off
/// (used by the login page to decide whether to render the
/// email/password inputs at all).
pub(in crate::admin::handlers) fn all_disable_local(state: &AdminState) -> bool {
    let auth_collections: Vec<_> = state
        .infra
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
        .all(|def| !def.auth.as_ref().is_some_and(Auth::password_login_enabled))
}

/// Check if "forgot password?" link should show on login page.
pub(in crate::admin::handlers) fn show_forgot_password(state: &AdminState) -> bool {
    if !email::is_configured(&state.config.email) {
        return false;
    }

    state
        .infra
        .registry
        .collections
        .values()
        .filter(|def| def.is_auth_collection())
        .any(|def| def.auth.as_ref().is_some_and(Auth::forgot_password_enabled))
}

pub(in crate::admin::handlers) fn get_auth_collections(state: &AdminState) -> Vec<AuthCollection> {
    let mut collections: Vec<AuthCollection> = state
        .infra
        .registry
        .collections
        .values()
        .filter(|def| def.is_auth_collection())
        .map(|def| AuthCollection {
            slug: def.slug.to_string(),
            display_name: def.display_name().to_string(),
        })
        .collect();

    collections.sort_by(|a, b| a.slug.cmp(&b.slug));

    collections
}

pub(in crate::admin::handlers) fn render_forgot_success(
    state: &AdminState,
    auth_collections: &[AuthCollection],
) -> Response {
    let show_collection_picker = auth_collections.len() > 1;

    let ctx = ForgotPasswordPage {
        base: AuthBasePageContext::for_state(
            state,
            PageMeta::new(PageType::AuthForgot, "Forgot Password"),
        ),
        success: true,
        collections: auth_collections.to_vec(),
        show_collection_picker,
    };

    render_page(state, HxNav::full(), "auth/forgot_password", &ctx)
}

/// Convert axum `HeaderMap` to a simple `HashMap<String, String>`.
pub(in crate::admin::handlers) fn headers_to_map(headers: &HeaderMap) -> HashMap<String, String> {
    headers
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_string())))
        .collect()
}

/// The single auth collection's slug, or `None` when there are zero or 2+.
///
/// The legacy un-scoped OAuth callback (`/admin/auth/callback/{name}`) can only
/// safely bind a session when the target collection is unambiguous. With 2+ auth
/// collections the callback cannot know which one to bind, so it fails closed and
/// the operator must use the collection-scoped route
/// (`/admin/auth/callback/{collection}/{name}`) instead. Either way the
/// hook-returned user must exist in the bound collection — the callback never
/// binds to a different auth collection by id.
pub(in crate::admin::handlers) fn sole_auth_collection(registry: &Registry) -> Option<String> {
    let mut auth = registry
        .collections
        .iter()
        .filter(|(_, d)| d.is_auth_collection())
        .map(|(slug, _)| slug.to_string());

    let only = auth.next()?;

    // 2+ auth collections → ambiguous; force the collection-scoped route.
    if auth.next().is_some() {
        return None;
    }

    Some(only)
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
///
/// `auth_time` is the Unix timestamp of the **original** authentication —
/// login paths pass `now()`, the refresh handler forwards the previous
/// token's `auth_time`. This lets `auth.session_absolute_max_age` cap
/// cumulative session lifetime independently of how often the token has
/// been refreshed.
pub(in crate::admin::handlers) fn create_session_token(
    state: &AdminState,
    user_id: String,
    collection: &str,
    email: String,
    session_version: u64,
    auth_time: u64,
) -> Result<SessionToken, String> {
    let expiry = state
        .infra
        .registry
        .get_collection(collection)
        .and_then(|def| def.auth.as_ref().map(|a| a.token_expiry))
        .unwrap_or(state.config.auth.token_expiry);

    let claims = ClaimsBuilder::new(user_id, Slug::new(collection))
        .email(email)
        .exp((Utc::now().timestamp().max(0).cast_unsigned()).saturating_add(expiry))
        .auth_time(auth_time)
        .session_version(session_version)
        .build()
        .map_err(|e| format!("Claims build error: {e}"))?;

    let token = state
        .infra
        .token_provider
        .create_token(&claims)
        .map_err(|e| format!("Token creation error: {e}"))?;

    Ok(SessionToken {
        token,
        expiry,
        exp: claims.exp,
    })
}

/// Build a redirect-to-/admin response with session cookies set.
pub(in crate::admin::handlers) fn session_redirect(
    state: &AdminState,
    session: &SessionToken,
) -> Response {
    let dev_mode = state.config.admin.dev_mode;
    let same_site = session_same_site(state);
    let cookies = session_cookies(
        &session.token,
        session.expiry,
        session.exp,
        dev_mode,
        same_site,
    );
    let mut response = Redirect::to("/admin").into_response();

    append_cookies(&mut response, &cookies);

    response
}

/// Extract the `crap_mfa_pending` cookie value from request headers. Shared by
/// the MFA page (GET) and the verify action (POST).
pub(in crate::admin::handlers) fn extract_mfa_token(headers: &HeaderMap) -> Option<String> {
    let cookie_header = headers.get(COOKIE)?.to_str().ok()?;

    extract_cookie(cookie_header, MFA_PENDING_COOKIE).map(std::string::ToString::to_string)
}

/// Render the MFA code entry form with an optional error message. Shared by the
/// MFA page (GET) and the verify action (POST, on re-render).
pub(in crate::admin::handlers) fn render_mfa_form(
    state: &AdminState,
    error: Option<&str>,
) -> Response {
    let ctx = MfaPage {
        base: AuthBasePageContext::for_state(
            state,
            PageMeta::new(PageType::AuthMfa, "mfa_page_title"),
        ),
        error: error.map(str::to_string),
    };

    render_page(state, HxNav::full(), "auth/mfa", &ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_mfa_token_present() {
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            "crap_csrf=abc; crap_mfa_pending=tok123; other=val"
                .parse()
                .unwrap(),
        );
        assert_eq!(extract_mfa_token(&headers), Some("tok123".to_string()));
    }

    #[test]
    fn extract_mfa_token_missing() {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, "crap_csrf=abc; other=val".parse().unwrap());
        assert_eq!(extract_mfa_token(&headers), None);
    }

    #[test]
    fn extract_mfa_token_no_cookie_header() {
        let headers = HeaderMap::new();
        assert_eq!(extract_mfa_token(&headers), None);
    }

    /// Helper that mirrors the legacy "trust XFF from anyone" behaviour —
    /// equivalent to `trusted_proxies = ["*"]` in `crap.toml`. Used to keep
    /// the pre-existing tests focused on XFF parsing, not on allowlist
    /// membership (which has its own tests below).
    fn trust_all() -> ServerConfig {
        trust_proxies(&["*"])
    }

    fn trust_proxies(entries: &[&str]) -> ServerConfig {
        ServerConfig {
            trust_proxy: true,
            trusted_proxies: entries
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
            ..ServerConfig::default()
        }
    }

    #[test]
    fn client_ip_trust_proxy_reads_xff() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "10.0.0.1, 192.168.1.1".parse().unwrap());
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        assert_eq!(client_ip(&headers, &addr, &trust_all()), "10.0.0.1");
    }

    #[test]
    fn client_ip_no_trust_proxy_ignores_xff() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "10.0.0.1, 192.168.1.1".parse().unwrap());
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        assert_eq!(
            client_ip(&headers, &addr, &ServerConfig::default()),
            "127.0.0.1"
        );
    }

    #[test]
    fn client_ip_falls_back_to_addr() {
        let headers = HeaderMap::new();
        let addr: SocketAddr = "192.168.1.5:5678".parse().unwrap();
        assert_eq!(client_ip(&headers, &addr, &trust_all()), "192.168.1.5");
    }

    #[test]
    fn client_ip_ignores_empty_xff() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "".parse().unwrap());
        let addr: SocketAddr = "10.0.0.2:80".parse().unwrap();
        assert_eq!(client_ip(&headers, &addr, &trust_all()), "10.0.0.2");
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
        assert_eq!(client_ip(&headers, &addr, &trust_all()), "2001:db8::1");
    }

    #[test]
    fn client_ip_handles_unparseable_xff() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "not-an-ip".parse().unwrap());
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        // Falls back to socket address when XFF is unparseable (prevents rate limiter bypass)
        assert_eq!(client_ip(&headers, &addr, &trust_all()), "127.0.0.1");
    }

    // ── H-3: trusted_proxies allowlist ────────────────────────────────────

    #[test]
    fn client_ip_allowlist_honors_xff_from_trusted_peer() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.5".parse().unwrap());
        let addr: SocketAddr = "10.0.0.5:80".parse().unwrap();
        let cfg = trust_proxies(&["10.0.0.0/8"]);
        assert_eq!(client_ip(&headers, &addr, &cfg), "203.0.113.5");
    }

    #[test]
    fn client_ip_allowlist_rejects_xff_from_untrusted_peer() {
        // Peer IP (1.2.3.4) is NOT in the allowlist, so any XFF it supplies
        // must be ignored — otherwise an attacker hitting us directly could
        // claim any client IP.
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.5".parse().unwrap());
        let addr: SocketAddr = "1.2.3.4:80".parse().unwrap();
        let cfg = trust_proxies(&["10.0.0.0/8"]);
        assert_eq!(client_ip(&headers, &addr, &cfg), "1.2.3.4");
    }

    #[test]
    fn client_ip_allowlist_supports_exact_ip_entries() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.5".parse().unwrap());
        let addr: SocketAddr = "127.0.0.1:80".parse().unwrap();
        let cfg = trust_proxies(&["127.0.0.1"]);
        assert_eq!(client_ip(&headers, &addr, &cfg), "203.0.113.5");
    }

    #[test]
    fn client_ip_allowlist_supports_ipv6_cidr() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "2001:db8::abcd".parse().unwrap());
        let addr: SocketAddr = "[::1]:80".parse().unwrap();
        let cfg = trust_proxies(&["::1/128"]);
        assert_eq!(client_ip(&headers, &addr, &cfg), "2001:db8::abcd");
    }

    #[test]
    fn client_ip_malformed_allowlist_entry_is_ignored() {
        // Malformed entries behave as "not trusted" at runtime. Startup
        // validation refuses to accept them in the first place; this
        // test documents the defensive runtime behaviour for robustness.
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.5".parse().unwrap());
        let addr: SocketAddr = "10.0.0.5:80".parse().unwrap();
        let cfg = trust_proxies(&["not-a-cidr", "10.0.0.0/8"]);
        assert_eq!(client_ip(&headers, &addr, &cfg), "203.0.113.5");
    }

    #[test]
    fn client_ip_wildcard_trusts_any_peer() {
        // Opt-in wildcard restores the legacy "trust XFF from anyone"
        // behaviour for deployments that need it (dev, test, isolated
        // networks). Startup validation warns about this setting.
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.5".parse().unwrap());
        let addr: SocketAddr = "1.2.3.4:80".parse().unwrap();
        let cfg = trust_proxies(&["*"]);
        assert_eq!(client_ip(&headers, &addr, &cfg), "203.0.113.5");
    }
}
