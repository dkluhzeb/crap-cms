//! The API-key guard every /mcp request passes, rate-limited per client.

use std::net::SocketAddr;

use axum::{
    Json,
    body::Body,
    http::{Request, StatusCode, header::AUTHORIZATION},
    response::{IntoResponse, Response},
};
use subtle::ConstantTimeEq;
use tracing::warn;

use crate::{
    admin::AdminState,
    config::McpApiKey,
    core::{
        ClientIp,
        rate_limit::{IP_MCP_API_KEY_KEYSPACE, LoginRateLimiter},
    },
    mcp::{INVALID_REQUEST, JsonRpcResponse},
};

/// A JSON-RPC error answer carrying `status`.
fn rpc_error(status: StatusCode, message: &str) -> Response {
    let body = Json(JsonRpcResponse::error(None, INVALID_REQUEST, message));

    (status, body).into_response()
}

/// Whether the `Authorization` header carries `Bearer <expected_key>`,
/// compared in constant time.
fn key_matches(request: &Request<Body>, expected_key: &McpApiKey) -> bool {
    let auth_header = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // `Display` on McpApiKey is redacted — the compare path must go through
    // `AsRef<str>` to see the real key.
    let expected = format!("Bearer {}", AsRef::<str>::as_ref(expected_key));

    bool::from(auth_header.as_bytes().ct_eq(expected.as_bytes()))
}

/// Check the API key under the client's failed-attempt budget.
///
/// A client whose failures reached the budget is refused (`429`) before its
/// key is even compared, until the window passes. A failure counts against
/// the client's rate-limit bucket (an IPv6 client per /64); a success clears
/// it. Failures are logged at `warn` with the client address and whether an
/// `Authorization` header was sent — a brute-force signal that never logs the
/// attempted key. The limiter fails closed, like every login limiter.
fn guard_api_key(
    limiter: &LoginRateLimiter,
    request: &Request<Body>,
    expected_key: &McpApiKey,
    client: &ClientIp,
) -> Result<(), Box<Response>> {
    let bucket = client.rate_limit_key();

    if limiter.is_blocked(&bucket) {
        warn!(peer = %client, "MCP HTTP auth refused: too many failed attempts");

        return Err(Box::new(rpc_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed authentication attempts — try again later",
        )));
    }

    if key_matches(request, expected_key) {
        limiter.clear(&bucket);

        return Ok(());
    }

    limiter.record_failure(&bucket);

    warn!(
        peer = %client,
        header_present = request.headers().contains_key(AUTHORIZATION),
        "MCP HTTP auth failed",
    );

    Err(Box::new(rpc_error(
        StatusCode::OK,
        "Invalid or missing API key",
    )))
}

/// Shared auth guard for both /mcp methods: empty-key defense-in-depth plus
/// the rate-limited constant-time API-key check. The failed-attempt budget is
/// the per-IP login budget's size (`[auth] max_ip_login_attempts` within
/// `login_lockout_seconds`) in its own keyspace, so MCP failures and admin
/// logins never drain each other.
pub(super) fn check_mcp_auth(
    state: &AdminState,
    request: &Request<Body>,
    peer_addr: SocketAddr,
) -> Result<(), Box<Response>> {
    // Defense-in-depth: reject all requests when no API key is configured.
    // Config validation already rejects this at startup, but a belt-and-braces
    // guard prevents accidental activation in tests or after a config reload.
    if state.config.mcp.api_key.is_empty() {
        return Err(Box::new(rpc_error(
            StatusCode::OK,
            "MCP HTTP endpoint requires an API key — set mcp.api_key in crap.toml",
        )));
    }

    let client = ClientIp::resolve(request.headers(), peer_addr.ip(), &state.config.server);
    let limiter = state.ip_login_limiter.rescoped(IP_MCP_API_KEY_KEYSPACE);

    guard_api_key(&limiter, request, &state.config.mcp.api_key, &client)
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    const KEY: &str = "0123456789abcdef0123456789abcdef";

    fn request_with_auth(header: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri("/mcp").method("POST");

        if let Some(h) = header {
            builder = builder.header(AUTHORIZATION, h);
        }

        builder.body(Body::empty()).unwrap()
    }

    fn client(last: u8) -> ClientIp {
        ClientIp::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, last)))
    }

    #[test]
    fn a_matching_bearer_is_accepted() {
        let req = request_with_auth(Some(&format!("Bearer {KEY}")));

        assert!(key_matches(&req, &McpApiKey::from(KEY)));
    }

    #[test]
    fn a_wrong_key_is_rejected() {
        let req = request_with_auth(Some("Bearer wrong-key"));

        assert!(!key_matches(&req, &McpApiKey::from(KEY)));
    }

    #[test]
    fn a_missing_header_is_rejected() {
        assert!(!key_matches(
            &request_with_auth(None),
            &McpApiKey::from(KEY)
        ));
    }

    /// The raw key without the `Bearer ` prefix must not authenticate.
    #[test]
    fn a_key_without_the_bearer_prefix_is_rejected() {
        let req = request_with_auth(Some(KEY));

        assert!(!key_matches(&req, &McpApiKey::from(KEY)));
    }

    /// The constant-time comparison must not short-circuit on a length
    /// mismatch in a way that exposes the expected key length.
    #[test]
    fn a_key_of_another_length_is_rejected() {
        let req = request_with_auth(Some("Bearer short"));

        assert!(!key_matches(&req, &McpApiKey::from(KEY)));
    }

    /// Regression: the MCP endpoint compared any number of guessed keys from
    /// one address — nothing slowed a brute force down. A client whose
    /// failures reach the budget is refused, even with the right key, until
    /// the window passes; other clients are unaffected.
    #[test]
    fn failed_attempts_lock_the_client_out() {
        let limiter = LoginRateLimiter::new(3, 60);
        let key = McpApiKey::from(KEY);
        let wrong = request_with_auth(Some("Bearer wrong"));
        let right = request_with_auth(Some(&format!("Bearer {KEY}")));

        for _ in 0..3 {
            let refused = guard_api_key(&limiter, &wrong, &key, &client(1)).unwrap_err();
            assert_eq!(refused.status(), StatusCode::OK);
        }

        let locked = guard_api_key(&limiter, &right, &key, &client(1)).unwrap_err();
        assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);

        assert!(guard_api_key(&limiter, &right, &key, &client(2)).is_ok());
    }

    /// A success clears the client's failures, so a client that mistyped its
    /// key once keeps its full budget.
    #[test]
    fn a_success_clears_the_failures() {
        let limiter = LoginRateLimiter::new(3, 60);
        let key = McpApiKey::from(KEY);
        let wrong = request_with_auth(Some("Bearer wrong"));
        let right = request_with_auth(Some(&format!("Bearer {KEY}")));

        for _ in 0..2 {
            let _ = guard_api_key(&limiter, &wrong, &key, &client(1));
        }
        assert!(guard_api_key(&limiter, &right, &key, &client(1)).is_ok());

        for _ in 0..2 {
            let _ = guard_api_key(&limiter, &wrong, &key, &client(1));
        }
        assert!(
            guard_api_key(&limiter, &right, &key, &client(1)).is_ok(),
            "the earlier failures were cleared"
        );
    }
}
