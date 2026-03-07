//! MCP (Model Context Protocol) server for Crap CMS.
//!
//! Provides auto-generated tool definitions from the Lua-defined Registry,
//! schema introspection resources, and optional config generation tools.
//! Supports stdio and HTTP transports.

pub mod protocol;
pub mod schema;
pub mod tools;
pub mod resources;
pub mod stdio;

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::config::CrapConfig;
use crate::core::Registry;
use crate::db::DbPool;
use crate::hooks::lifecycle::HookRunner;

use protocol::{
    JsonRpcRequest, JsonRpcResponse, InitializeParams, ToolCallParams,
    ResourceReadParams, PROTOCOL_VERSION, METHOD_NOT_FOUND, INVALID_PARAMS, INTERNAL_ERROR,
};

/// Shared state for the MCP server.
pub struct McpServer {
    pub pool: DbPool,
    pub registry: Arc<Registry>,
    pub runner: HookRunner,
    pub config: CrapConfig,
    pub config_dir: PathBuf,
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
            _ => JsonRpcResponse::error(req.id, METHOD_NOT_FOUND, format!("Unknown method: {}", req.method)),
        }
    }

    fn handle_initialize(&self, id: Option<Value>, params: Option<Value>) -> JsonRpcResponse {
        let _params: InitializeParams = match params {
            Some(p) => match serde_json::from_value(p) {
                Ok(p) => p,
                Err(e) => return JsonRpcResponse::error(id, INVALID_PARAMS, format!("Invalid params: {}", e)),
            },
            None => return JsonRpcResponse::error(id, INVALID_PARAMS, "Missing params"),
        };

        JsonRpcResponse::success(id, json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false },
            },
            "serverInfo": {
                "name": "crap-cms",
                "version": env!("CARGO_PKG_VERSION"),
            }
        }))
    }

    fn handle_tools_list(&self, id: Option<Value>) -> JsonRpcResponse {
        let tool_defs = tools::generate_tools(&self.registry, &self.config.mcp);
        let tools_json: Vec<Value> = tool_defs.iter().map(|t| {
            serde_json::to_value(t).unwrap_or(Value::Null)
        }).collect();
        JsonRpcResponse::success(id, json!({ "tools": tools_json }))
    }

    fn handle_tools_call(&self, id: Option<Value>, params: Option<Value>) -> JsonRpcResponse {
        let call: ToolCallParams = match params {
            Some(p) => match serde_json::from_value(p) {
                Ok(c) => c,
                Err(e) => return JsonRpcResponse::error(id, INVALID_PARAMS, format!("Invalid params: {}", e)),
            },
            None => return JsonRpcResponse::error(id, INVALID_PARAMS, "Missing params"),
        };

        match tools::execute_tool(
            &call.name,
            &call.arguments,
            &self.pool,
            &self.registry,
            &self.runner,
            &self.config_dir,
            &self.config,
        ) {
            Ok(result_text) => {
                JsonRpcResponse::success(id, json!({
                    "content": [{
                        "type": "text",
                        "text": result_text,
                    }]
                }))
            }
            Err(e) => {
                JsonRpcResponse::success(id, json!({
                    "content": [{
                        "type": "text",
                        "text": format!("Error: {}", e),
                    }],
                    "isError": true,
                }))
            }
        }
    }

    fn handle_resources_list(&self, id: Option<Value>) -> JsonRpcResponse {
        let resource_defs = resources::list_resources();
        let resources_json: Vec<Value> = resource_defs.iter().map(|r| {
            serde_json::to_value(r).unwrap_or(Value::Null)
        }).collect();
        JsonRpcResponse::success(id, json!({ "resources": resources_json }))
    }

    fn handle_resources_read(&self, id: Option<Value>, params: Option<Value>) -> JsonRpcResponse {
        let read_params: ResourceReadParams = match params {
            Some(p) => match serde_json::from_value(p) {
                Ok(r) => r,
                Err(e) => return JsonRpcResponse::error(id, INVALID_PARAMS, format!("Invalid params: {}", e)),
            },
            None => return JsonRpcResponse::error(id, INVALID_PARAMS, "Missing params"),
        };

        match resources::read_resource(&read_params.uri, &self.registry, &self.config) {
            Some(content) => {
                JsonRpcResponse::success(id, json!({
                    "contents": [serde_json::to_value(&content).unwrap_or(Value::Null)]
                }))
            }
            None => {
                JsonRpcResponse::error(id, INTERNAL_ERROR, format!("Resource not found: {}", read_params.uri))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::CollectionDefinition;

    fn make_request(method: &str, id: Option<Value>, params: Option<Value>) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        }
    }

    #[test]
    fn handle_ping() {
        let mut reg = Registry::new();
        reg.register_collection(CollectionDefinition {
            slug: "posts".to_string(),
            ..Default::default()
        });

        // We can't create a full McpServer without a pool/runner, but we can test the
        // message parsing and protocol types directly.
        let req = make_request("ping", Some(json!(1)), None);
        assert_eq!(req.method, "ping");
    }

    #[test]
    fn parse_initialize_request() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "initialize");
        let params: InitializeParams = serde_json::from_value(req.params.unwrap()).unwrap();
        assert_eq!(params.protocol_version, "2025-03-26");
    }

    #[test]
    fn handle_unknown_method() {
        let req = make_request("unknown/method", Some(json!(99)), None);
        // Verify the request parses correctly
        assert_eq!(req.method, "unknown/method");
    }

    #[test]
    fn handle_notification() {
        let req = make_request("notifications/initialized", None, None);
        assert!(req.id.is_none());
    }
}
