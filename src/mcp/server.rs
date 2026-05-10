//! `McpServer` struct and JSON-RPC message dispatch.

use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use serde::de::DeserializeOwned;
use serde_json::{Value, from_value, json, to_value};
use tracing::info;

use crate::{
    config::CrapConfig,
    core::{
        Registry,
        cache::SharedCache,
        event::{SharedEventTransport, SharedInvalidationTransport},
    },
    db::DbPool,
    hooks::HookRunner,
};

use super::protocol::{
    INTERNAL_ERROR, INVALID_PARAMS, InitializeParams, JsonRpcRequest, JsonRpcResponse,
    METHOD_NOT_FOUND, PROTOCOL_VERSION, ResourceReadParams, ToolCallParams,
};
use super::{
    resources,
    tools::{self, ToolExecCtx},
};

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
    /// Shared cross-request cache for cache invalidation on write ops.
    /// `None` = no cache invalidation (standalone CLI / tests).
    pub cache: Option<SharedCache>,
    /// Client name from the MCP `initialize` handshake. One-shot — the
    /// spec mandates `initialize` happens exactly once per session, so
    /// later calls are silently ignored. `get()` returns `None` until
    /// the first `initialize` lands; transports without per-session
    /// state (HTTP) won't ever populate it, which is why
    /// [`Self::transport_label`] exists as a fallback for audit logs.
    pub client_name: OnceLock<String>,
    /// Fallback identifier for audit logs when no client name is
    /// known yet. Set at construction by the transport runner —
    /// `"(stdio)"` for the long-lived stdio process, `"(http)"`
    /// for the per-request HTTP handler, `"(test)"` for unit tests.
    /// The parens disambiguate the fallback from a real client that
    /// happens to be named `stdio`/`http`/`test`.
    pub transport_label: &'static str,
}

impl McpServer {
    /// Resolve the audit-log identifier for the current call —
    /// the client name from `initialize` if present, otherwise the
    /// transport-level fallback.
    pub(in crate::mcp) fn audit_label(&self) -> &str {
        self.client_name
            .get()
            .map(String::as_str)
            .unwrap_or(self.transport_label)
    }
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
        let params: InitializeParams = match parse_params(&id, params) {
            Ok(p) => p,
            Err(resp) => return *resp,
        };

        let (client_name, client_version) = match params.client_info.as_ref() {
            Some(c) => (c.name.as_str(), c.version.as_deref().unwrap_or("?")),
            None => ("(unnamed)", "?"),
        };

        // Remember the client name for subsequent audit-log lines. Per
        // MCP spec `initialize` happens once per session, so a second
        // call here is a protocol violation — silently ignore the set
        // failure and keep the original name.
        let _ = self.client_name.set(client_name.to_string());

        info!(
            "MCP initialize: client={}/{} protocol={} capabilities={}",
            client_name, client_version, params.protocol_version, params.capabilities
        );

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

        let exec_ctx = ToolExecCtx {
            registry: &self.registry,
            pool: &self.pool,
            runner: &self.runner,
            config: &self.config,
            event_transport: self.event_transport.clone(),
            invalidation_transport: self.invalidation_transport.clone(),
            cache: self.cache.clone(),
            client_label: self.audit_label(),
        };
        let result = tools::execute_tool(&call.name, &call.arguments, &self.config_dir, &exec_ctx);

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
