//! `McpServer` struct and JSON-RPC message dispatch.

use std::{path::PathBuf, sync::Arc};

use serde::de::DeserializeOwned;
use serde_json::{Value, from_value, json, to_value};

use crate::{
    config::CrapConfig,
    core::{
        Registry,
        event::{SharedEventTransport, SharedInvalidationTransport},
    },
    db::DbPool,
    hooks::HookRunner,
};

use super::protocol::{
    INTERNAL_ERROR, INVALID_PARAMS, InitializeParams, JsonRpcRequest, JsonRpcResponse,
    METHOD_NOT_FOUND, PROTOCOL_VERSION, ResourceReadParams, ToolCallParams,
};
use super::{resources, tools};

/// Shared state for the MCP server.
pub struct McpServer {
    pub pool: DbPool,
    pub registry: Arc<Registry>,
    pub runner: HookRunner,
    pub config: CrapConfig,
    pub config_dir: PathBuf,
    /// Transport for publishing mutation events to live-update subscribers.
    pub event_transport: Option<SharedEventTransport>,
    /// Transport for publishing user-invalidation signals on hard-delete
    /// of auth documents. `None` = no-op (MCP built in isolation / tests).
    pub invalidation_transport: Option<SharedInvalidationTransport>,
}

/// Parse required JSON-RPC params, returning an error response on failure.
fn parse_params<T: DeserializeOwned>(
    id: &Option<Value>,
    params: Option<Value>,
) -> Result<T, Box<JsonRpcResponse>> {
    let Some(p) = params else {
        return Err(Box::new(JsonRpcResponse::error(
            id.clone(),
            INVALID_PARAMS,
            "Missing params",
        )));
    };

    from_value(p).map_err(|e| {
        Box::new(JsonRpcResponse::error(
            id.clone(),
            INVALID_PARAMS,
            format!("Invalid params: {e}"),
        ))
    })
}

impl McpServer {
    /// Handle a single JSON-RPC request and return a response.
    pub fn handle_message(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        match req.method.as_str() {
            "initialize" => self.handle_initialize(req.id, req.params),
            "notifications/initialized" => {
                // Client acknowledgement — no response needed
                JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: None,
                    result: None,
                    error: None,
                }
            }
            "tools/list" => self.handle_tools_list(req.id),
            "tools/call" => self.handle_tools_call(req.id, req.params),
            "resources/list" => self.handle_resources_list(req.id),
            "resources/read" => self.handle_resources_read(req.id, req.params),
            "ping" => JsonRpcResponse::success(req.id, json!({})),
            _ => JsonRpcResponse::error(
                req.id,
                METHOD_NOT_FOUND,
                format!("Unknown method: {}", req.method),
            ),
        }
    }

    /// Respond with server capabilities and protocol version.
    fn handle_initialize(&self, id: Option<Value>, params: Option<Value>) -> JsonRpcResponse {
        let _params: InitializeParams = match parse_params(&id, params) {
            Ok(p) => p,
            Err(resp) => return *resp,
        };

        JsonRpcResponse::success(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {
                    "tools": { "listChanged": false },
                    "resources": { "subscribe": false, "listChanged": false },
                },
                "serverInfo": {
                    "name": "crap-cms",
                    "version": env!("CARGO_PKG_VERSION"),
                }
            }),
        )
    }

    /// List all available MCP tools.
    fn handle_tools_list(&self, id: Option<Value>) -> JsonRpcResponse {
        let tool_defs = tools::generate_tools(&self.registry, &self.config.mcp);
        let tools_json: Vec<Value> = tool_defs
            .iter()
            .map(|t| to_value(t).unwrap_or(Value::Null))
            .collect();

        JsonRpcResponse::success(id, json!({ "tools": tools_json }))
    }

    /// Execute a tool call and return the result.
    fn handle_tools_call(&self, id: Option<Value>, params: Option<Value>) -> JsonRpcResponse {
        let call: ToolCallParams = match parse_params(&id, params) {
            Ok(c) => c,
            Err(resp) => return *resp,
        };

        let result = tools::execute_tool(
            &call.name,
            &call.arguments,
            &self.pool,
            &self.registry,
            &self.runner,
            &self.config_dir,
            &self.config,
            self.event_transport.clone(),
            self.invalidation_transport.clone(),
        );

        match result {
            Ok(text) => JsonRpcResponse::success(
                id,
                json!({ "content": [{ "type": "text", "text": text }] }),
            ),
            Err(e) => JsonRpcResponse::success(
                id,
                json!({
                    "content": [{ "type": "text", "text": format!("Error: {e}") }],
                    "isError": true,
                }),
            ),
        }
    }

    /// List all available MCP resources.
    fn handle_resources_list(&self, id: Option<Value>) -> JsonRpcResponse {
        let resource_defs = resources::list_resources();
        let resources_json: Vec<Value> = resource_defs
            .iter()
            .map(|r| to_value(r).unwrap_or(Value::Null))
            .collect();

        JsonRpcResponse::success(id, json!({ "resources": resources_json }))
    }

    /// Read a single resource by URI.
    fn handle_resources_read(&self, id: Option<Value>, params: Option<Value>) -> JsonRpcResponse {
        let read_params: ResourceReadParams = match parse_params(&id, params) {
            Ok(r) => r,
            Err(resp) => return *resp,
        };

        let Some(content) =
            resources::read_resource(&read_params.uri, &self.registry, &self.config)
        else {
            return JsonRpcResponse::error(
                id,
                INTERNAL_ERROR,
                format!("Resource not found: {}", read_params.uri),
            );
        };

        JsonRpcResponse::success(
            id,
            json!({ "contents": [to_value(&content).unwrap_or(Value::Null)] }),
        )
    }
}
