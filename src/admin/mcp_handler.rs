//! MCP HTTP transport handler — JSON-RPC 2.0 over POST /mcp.

use axum::{
    Json,
    body::{self, Body},
    extract::State,
    http::{Request, StatusCode, header::AUTHORIZATION},
    response::{IntoResponse, Response},
};
use subtle::ConstantTimeEq;
use tokio::task;

use crate::{
    admin::AdminState,
    config::McpApiKey,
    mcp::{
        McpServer,
        protocol::{
            INTERNAL_ERROR, INVALID_REQUEST, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
            PARSE_ERROR,
        },
    },
};

/// Validate the API key from the Authorization header.
fn validate_api_key(
    request: &Request<Body>,
    expected_key: &McpApiKey,
) -> Result<(), Box<Response>> {
    let auth_header = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let expected = format!("Bearer {expected_key}");
    let is_valid = auth_header.as_bytes().ct_eq(expected.as_bytes());

    if bool::from(is_valid) {
        Ok(())
    } else {
        Err(Box::new(
            Json(JsonRpcResponse::error(
                None,
                INVALID_REQUEST,
                "Invalid or missing API key",
            ))
            .into_response(),
        ))
    }
}

/// Parse the JSON-RPC request body.
async fn parse_rpc_body(request: Request<Body>) -> Result<JsonRpcRequest, Response> {
    let body_bytes = body::to_bytes(request.into_body(), 1024 * 1024)
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
                message: format!("Parse error: {}", e),
                data: None,
            }),
        })
        .into_response()
    })
}

/// MCP HTTP transport handler — receives JSON-RPC 2.0 over POST /mcp.
/// Validates API key from Authorization header.
#[cfg(not(tarpaulin_include))]
pub(super) async fn mcp_http_handler(
    State(state): State<AdminState>,
    request: Request<Body>,
) -> Response {
    // Defense-in-depth: reject all requests when no API key is configured.
    if state.config.mcp.api_key.is_empty() {
        return Json(JsonRpcResponse::error(
            None,
            INVALID_REQUEST,
            "MCP HTTP endpoint requires an API key — set mcp.api_key in crap.toml",
        ))
        .into_response();
    }

    if let Err(resp) = validate_api_key(&request, &state.config.mcp.api_key) {
        return *resp;
    }

    let rpc_request = match parse_rpc_body(request).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };

    let server = McpServer {
        pool: state.pool.clone(),
        registry: state.registry.clone(),
        runner: state.hook_runner.clone(),
        config: state.config.clone(),
        config_dir: state.config_dir.clone(),
        event_transport: state.event_transport.clone(),
        invalidation_transport: Some(state.invalidation_transport.clone()),
    };

    let response = match task::spawn_blocking(move || server.handle_message(rpc_request)).await {
        Ok(resp) => resp,
        Err(_) => JsonRpcResponse::error(None, INTERNAL_ERROR, "Internal error"),
    };

    // Notifications must not receive a response per JSON-RPC spec
    if response.id.is_none() && response.result.is_none() && response.error.is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }

    Json(response).into_response()
}
