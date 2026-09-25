//! POST /mcp — JSON-RPC 2.0 requests, single or batched.

use std::net::SocketAddr;

use axum::{
    Json,
    body::{self, Body},
    extract::{ConnectInfo, State},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, from_slice};
use tokio::task;

use super::{
    auth::check_mcp_auth,
    session::{SESSION_HEADER, adopt_session},
};
use crate::{
    admin::AdminState,
    mcp::{
        INTERNAL_ERROR, JsonRpcError, JsonRpcRequest, JsonRpcResponse, McpServer, PARSE_ERROR,
        batch::{Payload, classify, handle_batch},
    },
};

/// Answer a body that could not be read as JSON-RPC.
fn parse_error(message: impl Into<String>) -> Response {
    Json(JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: None,
        result: None,
        error: Some(JsonRpcError {
            code: PARSE_ERROR,
            message: message.into(),
            data: None,
        }),
    })
    .into_response()
}

/// Parse the JSON-RPC request body into a single request or a batch.
/// `max_body_bytes` comes from `[mcp] http_max_body_bytes` (default 1 MiB).
async fn parse_rpc_body(request: Request<Body>, max_body_bytes: u64) -> Result<Payload, Response> {
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

    let body: Value =
        from_slice(&body_bytes).map_err(|e| parse_error(format!("Parse error: {e}")))?;

    classify(body).map_err(|e| parse_error(format!("Parse error: {e}")))
}

/// Run a batch on the blocking pool. Every member is dispatched; the reply is
/// an array of the non-notification members' responses, or 204 when there is
/// nothing to send. `initialize` may not appear in a batch (MCP spec), so no
/// session is opened here.
async fn respond_to_batch(server: McpServer, members: Vec<Value>) -> Response {
    let Ok(out) = task::spawn_blocking(move || handle_batch(&server, members)).await else {
        return Json(JsonRpcResponse::error(
            None,
            INTERNAL_ERROR,
            "Internal error",
        ))
        .into_response();
    };

    match out {
        Some(value) => Json(value).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

/// Run a single request on the blocking pool, opening a tracked session when
/// it was the `initialize` handshake.
async fn respond_to_single(
    state: &AdminState,
    server: McpServer,
    rpc_request: JsonRpcRequest,
) -> Response {
    // A request with no `id` is a JSON-RPC notification — no response is sent
    // (spec: MUST NOT reply). Capture the id before the move so a join error
    // can still echo it.
    let is_notification = rpc_request.id.is_none();
    let request_id = rpc_request.id.clone();
    let is_initialize = rpc_request.method == "initialize";

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

/// MCP HTTP transport handler — receives JSON-RPC 2.0 over POST /mcp, as a
/// single request object or a batch array. Validates API key from the
/// Authorization header. `initialize` opens a tracked session (the response
/// carries `Mcp-Session-Id`); later requests echoing the header get their
/// audit identity resolved from it.
#[cfg(not(tarpaulin_include))]
pub(in crate::admin) async fn mcp_http_handler(
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

    let payload = match parse_rpc_body(request, state.config.mcp.http_max_body_bytes).await {
        Ok(p) => p,
        Err(resp) => return resp,
    };

    let server = state.mcp_server();

    match payload {
        Payload::Batch(members) => {
            adopt_session(&state, &server, session_id.as_deref());

            respond_to_batch(server, members).await
        }

        Payload::Single(rpc_request) => {
            if rpc_request.method != "initialize" {
                adopt_session(&state, &server, session_id.as_deref());
            }

            respond_to_single(&state, server, *rpc_request).await
        }
    }
}

#[cfg(test)]
mod tests {
    use toml::from_str as toml_from_str;

    use super::*;
    use crate::config::McpConfig;

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
        let cfg = McpConfig::default();
        assert_eq!(cfg.http_max_body_bytes, 1_048_576);

        let parsed: McpConfig = toml_from_str(r#"http_max_body_bytes = "16MB""#).unwrap();
        assert_eq!(parsed.http_max_body_bytes, 16 * 1024 * 1024);
    }
}
