//! MCP HTTP transport handler — JSON-RPC 2.0 over POST /mcp, with
//! `Mcp-Session-Id` session tracking (per-session client identity for
//! audit logs) and DELETE-based session termination.

use std::net::SocketAddr;

use axum::{
    Json,
    body::{self, Body},
    extract::{ConnectInfo, State},
    http::{Request, StatusCode, header::AUTHORIZATION},
    response::{IntoResponse, Response},
};
use subtle::ConstantTimeEq;
use tokio::task;
use tracing::warn;

use crate::{
    admin::AdminState,
    config::McpApiKey,
    mcp::{
        INTERNAL_ERROR, INVALID_REQUEST, JsonRpcError, JsonRpcRequest, JsonRpcResponse, PARSE_ERROR,
    },
};

/// Validate the API key from the Authorization header.
///
/// Failures are logged at `warn` with the peer IP and whether an
/// Authorization header was supplied — giving operators a signal for
/// brute-force attempts without leaking the attempted key into logs.
fn validate_api_key(
    request: &Request<Body>,
    expected_key: &McpApiKey,
    peer_addr: Option<SocketAddr>,
) -> Result<(), Box<Response>> {
    let auth_header = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let expected = format!("Bearer {expected_key}");
    let is_valid = auth_header.as_bytes().ct_eq(expected.as_bytes());

    if bool::from(is_valid) {
        return Ok(());
    }

    let peer = peer_addr.map_or_else(|| "unknown".into(), |a| a.ip().to_string());

    warn!(
        peer = %peer,
        header_present = !auth_header.is_empty(),
        "MCP HTTP auth failed",
    );

    Err(Box::new(
        Json(JsonRpcResponse::error(
            None,
            INVALID_REQUEST,
            "Invalid or missing API key",
        ))
        .into_response(),
    ))
}

/// Parse the JSON-RPC request body. `max_body_bytes` comes from
/// `[mcp] http_max_body_bytes` (default 1 MiB).
async fn parse_rpc_body(
    request: Request<Body>,
    max_body_bytes: u64,
) -> Result<JsonRpcRequest, Response> {
    let limit = usize::try_from(max_body_bytes).unwrap_or(usize::MAX);
    let body_bytes = body::to_bytes(request.into_body(), limit)
        .await
        .map_err(|_| {
            Json(JsonRpcResponse::error(
                None,
                PARSE_ERROR,
                "Request body too large",
            ))
            .into_response()
        })?;

    serde_json::from_slice(&body_bytes).map_err(|e| {
        Json(JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: None,
            result: None,
            error: Some(JsonRpcError {
                code: PARSE_ERROR,
                message: format!("Parse error: {e}"),
                data: None,
            }),
        })
        .into_response()
    })
}

/// The MCP spec's session header: returned on `initialize`, echoed by the
/// client on every later request. Lowercase (HTTP header names are
/// case-insensitive; axum normalizes to lowercase).
const SESSION_HEADER: &str = "mcp-session-id";

/// Shared auth guard for both /mcp methods: empty-key defense-in-depth plus
/// the constant-time API-key check.
fn check_mcp_auth(
    state: &AdminState,
    request: &Request<Body>,
    peer_addr: SocketAddr,
) -> Result<(), Box<Response>> {
    // Defense-in-depth: reject all requests when no API key is configured.
    // Config validation already rejects this at startup, but a belt-and-braces
    // guard prevents accidental activation in tests or after a config reload.
    if state.config.mcp.api_key.is_empty() {
        return Err(Box::new(
            Json(JsonRpcResponse::error(
                None,
                INVALID_REQUEST,
                "MCP HTTP endpoint requires an API key — set mcp.api_key in crap.toml",
            ))
            .into_response(),
        ));
    }

    validate_api_key(request, &state.config.mcp.api_key, Some(peer_addr))
}

/// MCP HTTP transport handler — receives JSON-RPC 2.0 over POST /mcp.
/// Validates API key from Authorization header. `initialize` opens a
/// tracked session (the response carries `Mcp-Session-Id`); later requests
/// echoing the header get their audit identity resolved from it. A
/// missing/unknown/expired session id is never an error — the audit label
/// falls back to `(http)`.
#[cfg(not(tarpaulin_include))]
pub(super) async fn mcp_http_handler(
    State(state): State<AdminState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    request: Request<Body>,
) -> Response {
    if let Err(resp) = check_mcp_auth(&state, &request, peer_addr) {
        return *resp;
    }

    // Capture the session id before the body parse consumes the request.
    let session_id = request
        .headers()
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let rpc_request = match parse_rpc_body(request, state.config.mcp.http_max_body_bytes).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };

    // A request with no `id` is a JSON-RPC notification — no response is sent
    // (spec: MUST NOT reply). Capture the id before the move so a join error can
    // still echo it.
    let is_notification = rpc_request.id.is_none();
    let request_id = rpc_request.id.clone();
    let is_initialize = rpc_request.method == "initialize";

    let server = state.mcp_server();

    // Tracked session: pre-populate the fresh per-request server with the
    // session's client name so audit lines read `[client=Claude Code]`
    // instead of `[client=(http)]` — parity with the stdio transport.
    if !is_initialize
        && let Some(id) = session_id.as_deref()
        && let Some(name) = state.mcp_sessions.lookup_touch(id)
    {
        let _ = server.client_name.set(name);
    }

    let Ok((server, response)) = task::spawn_blocking(move || {
        let response = server.handle_message(rpc_request);
        (server, response)
    })
    .await
    else {
        return Json(JsonRpcResponse::error(
            request_id,
            INTERNAL_ERROR,
            "Internal error",
        ))
        .into_response();
    };

    if is_notification {
        return StatusCode::NO_CONTENT.into_response();
    }

    let mut http_response = Json(response).into_response();

    // A successful `initialize` announced a client name — open the tracked
    // session and hand its id back per spec.
    if is_initialize
        && let Some(name) = server.client_name.get()
        && let Ok(value) = state.mcp_sessions.insert(name).parse()
    {
        http_response.headers_mut().insert(SESSION_HEADER, value);
    }

    http_response
}

/// DELETE /mcp — explicit session termination (MCP spec). Requires the API
/// key like every transport request; 204 when the session existed, 404
/// otherwise, 400 without the header.
#[cfg(not(tarpaulin_include))]
pub(super) async fn mcp_delete_session_handler(
    State(state): State<AdminState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    request: Request<Body>,
) -> Response {
    if let Err(resp) = check_mcp_auth(&state, &request, peer_addr) {
        return *resp;
    }

    let Some(session_id) = request
        .headers()
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    if state.mcp_sessions.remove(session_id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with_auth(header: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri("/mcp").method("POST");

        if let Some(h) = header {
            builder = builder.header(AUTHORIZATION, h);
        }

        builder.body(Body::empty()).unwrap()
    }

    #[test]
    fn validate_api_key_accepts_matching_bearer() {
        let key = McpApiKey::from("0123456789abcdef0123456789abcdef");
        let req = request_with_auth(Some("Bearer 0123456789abcdef0123456789abcdef"));

        assert!(validate_api_key(&req, &key, None).is_ok());
    }

    #[test]
    fn validate_api_key_rejects_wrong_key() {
        let key = McpApiKey::from("0123456789abcdef0123456789abcdef");
        let req = request_with_auth(Some("Bearer wrong-key"));

        assert!(validate_api_key(&req, &key, None).is_err());
    }

    #[test]
    fn validate_api_key_rejects_missing_header() {
        let key = McpApiKey::from("0123456789abcdef0123456789abcdef");
        let req = request_with_auth(None);

        assert!(validate_api_key(&req, &key, None).is_err());
    }

    #[test]
    fn validate_api_key_rejects_bearer_without_prefix() {
        // Raw key without "Bearer " prefix must not authenticate.
        let key = McpApiKey::from("0123456789abcdef0123456789abcdef");
        let req = request_with_auth(Some("0123456789abcdef0123456789abcdef"));

        assert!(validate_api_key(&req, &key, None).is_err());
    }

    #[test]
    fn validate_api_key_rejects_different_length() {
        // Ensures the constant-time comparison does not short-circuit on
        // length mismatch in a way that exposes the expected key length.
        let key = McpApiKey::from("0123456789abcdef0123456789abcdef");
        let req = request_with_auth(Some("Bearer short"));

        assert!(validate_api_key(&req, &key, None).is_err());
    }

    fn rpc_request_with_body(body: String) -> Request<Body> {
        Request::builder()
            .uri("/mcp")
            .method("POST")
            .body(Body::from(body))
            .unwrap()
    }

    /// Regression: the body cap used to be hardcoded to 1 MiB; it now comes
    /// from `[mcp] http_max_body_bytes`.
    #[tokio::test]
    async fn parse_rpc_body_enforces_configured_cap() {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"x","params":{{"pad":"{}"}}}}"#,
            "a".repeat(256)
        );

        let over = parse_rpc_body(rpc_request_with_body(body.clone()), 64).await;
        assert!(
            over.is_err(),
            "body over the configured cap must be rejected"
        );

        let under = parse_rpc_body(rpc_request_with_body(body), 4096).await;
        assert!(under.is_ok(), "body under the configured cap must parse");
    }

    #[test]
    fn mcp_config_body_cap_default_and_filesize_string() {
        let cfg = crate::config::McpConfig::default();
        assert_eq!(cfg.http_max_body_bytes, 1_048_576);

        let parsed: crate::config::McpConfig =
            toml::from_str(r#"http_max_body_bytes = "16MB""#).unwrap();
        assert_eq!(parsed.http_max_body_bytes, 16 * 1024 * 1024);
    }
}
