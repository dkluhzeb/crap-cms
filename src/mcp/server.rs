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
        Registry, SharedCache, SharedEventTransport, SharedInvalidationTransport, SharedStorage,
    },
    db::DbPool,
    hooks::HookRunner,
};

use super::protocol::{
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, InitializeParams, JsonRpcRequest,
    JsonRpcResponse, METHOD_NOT_FOUND, PROTOCOL_VERSION, ResourceReadParams, ToolCallParams,
};
use super::{
    access::McpExposure,
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
    /// Storage backend for deleting uploaded files on hard-delete.
    /// `None` = files are left in place (tests without an upload backend).
    pub storage: Option<SharedStorage>,
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
            .map_or(self.transport_label, String::as_str)
    }
}

/// Parse required JSON-RPC params, returning an error response on failure.
fn parse_params<T: DeserializeOwned>(
    id: Option<&Value>,
    params: Option<Value>,
) -> Result<T, Box<JsonRpcResponse>> {
    let Some(p) = params else {
        return Err(Box::new(JsonRpcResponse::error(
            id.cloned(),
            INVALID_PARAMS,
            "Missing params",
        )));
    };

    from_value(p).map_err(|e| {
        Box::new(JsonRpcResponse::error(
            id.cloned(),
            INVALID_PARAMS,
            format!("Invalid params: {e}"),
        ))
    })
}

impl McpServer {
    /// Handle a single JSON-RPC request and return a response.
    pub fn handle_message(&self, req: JsonRpcRequest) -> JsonRpcResponse {
        // JSON-RPC 2.0 requires the envelope to declare `"jsonrpc": "2.0"`.
        // Reject anything else with INVALID_REQUEST rather than tolerating it
        // (a reply to a notification is suppressed by the transport).
        if req.jsonrpc != "2.0" {
            return JsonRpcResponse::error(
                req.id,
                INVALID_REQUEST,
                format!(
                    "Unsupported jsonrpc version '{}'; expected \"2.0\"",
                    req.jsonrpc
                ),
            );
        }

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
            "resources/list" => Self::handle_resources_list(req.id),
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
        let params: InitializeParams = match parse_params(id.as_ref(), params) {
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

    /// Resolve `access.mcp` exposure for the current registry. Best-effort: if a
    /// connection can't be obtained, falls back to "expose all" — advertising
    /// only; the execution path (`execute_tool`) re-checks `access.mcp` against
    /// its own connection, so a hidden collection's data is never reachable even
    /// if it briefly remains advertised.
    fn mcp_exposure(&self) -> McpExposure {
        match self.pool.get() {
            Ok(conn) => McpExposure::resolve(&self.registry, &self.runner, &conn),
            Err(e) => {
                tracing::warn!("access.mcp exposure unresolved ({e}); advertising all collections");
                McpExposure::default()
            }
        }
    }

    /// List all available MCP tools.
    fn handle_tools_list(&self, id: Option<Value>) -> JsonRpcResponse {
        let exposure = self.mcp_exposure();
        let tool_defs = tools::generate_tools(&self.registry, &self.config.mcp, &exposure);
        let tools_json: Vec<Value> = tool_defs
            .iter()
            .map(|t| to_value(t).unwrap_or(Value::Null))
            .collect();

        JsonRpcResponse::success(id, json!({ "tools": tools_json }))
    }

    /// Execute a tool call and return the result.
    fn handle_tools_call(&self, id: Option<Value>, params: Option<Value>) -> JsonRpcResponse {
        let call: ToolCallParams = match parse_params(id.as_ref(), params) {
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
            storage: self.storage.clone(),
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
    fn handle_resources_list(id: Option<Value>) -> JsonRpcResponse {
        let resource_defs = resources::list_resources();
        let resources_json: Vec<Value> = resource_defs
            .iter()
            .map(|r| to_value(r).unwrap_or(Value::Null))
            .collect();

        JsonRpcResponse::success(id, json!({ "resources": resources_json }))
    }

    /// Read a single resource by URI.
    fn handle_resources_read(&self, id: Option<Value>, params: Option<Value>) -> JsonRpcResponse {
        let read_params: ResourceReadParams = match parse_params(id.as_ref(), params) {
            Ok(r) => r,
            Err(resp) => return *resp,
        };

        let exposure = self.mcp_exposure();
        let Some(content) =
            resources::read_resource(&read_params.uri, &self.registry, &self.config, &exposure)
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

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::items_after_statements,
    clippy::match_wildcard_for_single_variants,
    clippy::missing_panics_doc,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    clippy::used_underscore_binding
)]
mod tests {
    use std::sync::OnceLock;

    use serde_json::{Value, json};

    // `super::*` brings in McpServer + the protocol-type imports server.rs
    // already declares (INTERNAL_ERROR, INVALID_PARAMS, InitializeParams,
    // JsonRpcRequest, JsonRpcResponse, METHOD_NOT_FOUND, PROTOCOL_VERSION).
    use super::*;
    use crate::{
        config::CrapConfig,
        core::{
            Registry,
            collection::CollectionDefinition,
            field::{FieldDefinition, FieldType},
            upload::{CollectionUpload, storage::LocalStorage},
        },
        db::{migrate, pool},
        hooks::HookRunner,
    };

    fn make_request(method: &str, id: Option<Value>, params: Option<Value>) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        }
    }

    /// Build a full `McpServer` backed by a real `SQLite` pool and `HookRunner`.
    fn make_server_with(collections: Vec<CollectionDefinition>) -> (tempfile::TempDir, McpServer) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();

        let db_pool = pool::create_pool(tmp.path(), &config).expect("create pool");

        let shared = Registry::shared();
        {
            let mut reg = shared.write().unwrap();
            for def in &collections {
                reg.register_collection(def.clone());
            }
        }

        migrate::sync_all(&db_pool, &shared.read().unwrap(), &config.locale).expect("sync schema");

        let registry = Registry::snapshot(&shared);
        let runner = HookRunner::builder()
            .config_dir(tmp.path())
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .expect("hook runner");

        let server = McpServer {
            pool: db_pool,
            registry,
            runner,
            config,
            config_dir: tmp.path().to_path_buf(),
            event_transport: None,
            invalidation_transport: None,
            cache: None,
            // Real local-disk storage rooted at `<tmp>/uploads` so hard-delete
            // file cleanup is exercised end-to-end (mirrors production wiring).
            storage: Some(Arc::new(LocalStorage::new(tmp.path().join("uploads")))),
            client_name: OnceLock::new(),
            transport_label: "(test)",
        };
        (tmp, server)
    }

    fn make_server() -> (tempfile::TempDir, McpServer) {
        make_server_with(vec![CollectionDefinition::new("posts")])
    }

    // ── protocol type helpers ──────────────────────────────────────────────

    #[test]
    fn parse_initialize_request() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
        let req: JsonRpcRequest = serde_json::from_str(raw).unwrap();
        assert_eq!(req.method, "initialize");
        let params: InitializeParams = serde_json::from_value(req.params.unwrap()).unwrap();
        assert_eq!(params.protocol_version, "2025-03-26");
    }

    // ── handle_message routing ─────────────────────────────────────────────

    #[test]
    fn handle_ping_returns_success() {
        let (_tmp, server) = make_server();
        let req = make_request("ping", Some(json!(42)), None);
        let resp = server.handle_message(req);
        assert!(resp.error.is_none());
        assert!(resp.result.is_some());
        assert_eq!(resp.id, Some(json!(42)));
    }

    #[test]
    fn handle_unknown_method_returns_error() {
        let (_tmp, server) = make_server();
        let req = make_request("unknown/method", Some(json!(99)), None);
        let resp = server.handle_message(req);
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.code, METHOD_NOT_FOUND);
        assert!(err.message.contains("Unknown method"));
    }

    #[test]
    fn handle_notification_initialized_returns_no_id() {
        let (_tmp, server) = make_server();
        let req = make_request("notifications/initialized", None, None);
        let resp = server.handle_message(req);
        // Notification response: id=None, no result, no error
        assert!(resp.id.is_none());
        assert!(resp.result.is_none());
        assert!(resp.error.is_none());
    }

    #[test]
    fn handle_initialize_success() {
        let (_tmp, server) = make_server();
        let req = make_request(
            "initialize",
            Some(json!(1)),
            Some(json!({
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "test-client", "version": "0.1" }
            })),
        );
        let resp = server.handle_message(req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
        assert!(result["capabilities"].is_object());
        assert_eq!(result["serverInfo"]["name"], "crap-cms");
    }

    #[test]
    fn handle_initialize_missing_params_returns_error() {
        let (_tmp, server) = make_server();
        let req = make_request("initialize", Some(json!(2)), None);
        let resp = server.handle_message(req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    #[test]
    fn handle_initialize_invalid_params_returns_error() {
        let (_tmp, server) = make_server();
        // params is not an object matching InitializeParams
        let req = make_request("initialize", Some(json!(3)), Some(json!("not-an-object")));
        let resp = server.handle_message(req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    #[test]
    fn handle_tools_list_returns_tools() {
        let (_tmp, server) = make_server();
        let req = make_request("tools/list", Some(json!(5)), None);
        let resp = server.handle_message(req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        // Should have at least the introspection tools + collection CRUD tools
        assert!(!tools.is_empty());
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().unwrap_or(""))
            .collect();
        assert!(names.contains(&"list_collections"));
        assert!(names.contains(&"find_posts"));
    }

    #[test]
    fn handle_tools_call_list_collections() {
        let (_tmp, server) = make_server();
        let req = make_request(
            "tools/call",
            Some(json!(6)),
            Some(json!({
                "name": "list_collections",
                "arguments": {}
            })),
        );
        let resp = server.handle_message(req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let content = result["content"].as_array().unwrap();
        assert!(!content.is_empty());
        assert_eq!(content[0]["type"], "text");
        let text = content[0]["text"].as_str().unwrap();
        // Should contain "posts"
        assert!(text.contains("posts"));
    }

    #[test]
    fn handle_tools_call_unknown_tool_returns_is_error() {
        let (_tmp, server) = make_server();
        let req = make_request(
            "tools/call",
            Some(json!(7)),
            Some(json!({
                "name": "nonexistent_tool",
                "arguments": {}
            })),
        );
        let resp = server.handle_message(req);
        // Error during tool execution is returned as a success response with isError=true
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["isError"], true);
    }

    #[test]
    fn handle_tools_call_validate_reports_errors_without_persisting() {
        let mut def = CollectionDefinition::new("posts");
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text)
                .required(true)
                .build(),
        ];
        let (_tmp, server) = make_server_with(vec![def]);

        // Missing required `title` → valid:false with a per-field error.
        let req = make_request(
            "tools/call",
            Some(json!(40)),
            Some(json!({ "name": "validate_posts", "arguments": {} })),
        );
        let resp = server.handle_message(req);
        assert!(resp.error.is_none());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).expect("validate output is JSON");
        assert_eq!(parsed["valid"], false);
        assert!(
            parsed["errors"]["title"].is_string(),
            "missing required field should surface a per-field error: {parsed}"
        );

        // Valid data → valid:true.
        let req = make_request(
            "tools/call",
            Some(json!(41)),
            Some(json!({ "name": "validate_posts", "arguments": { "title": "Hello" } })),
        );
        let resp = server.handle_message(req);
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).expect("validate output is JSON");
        assert_eq!(parsed["valid"], true);

        // Validation must not have written a row — count stays 0.
        let req = make_request(
            "tools/call",
            Some(json!(42)),
            Some(json!({ "name": "count_posts", "arguments": {} })),
        );
        let resp = server.handle_message(req);
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let parsed: Value = serde_json::from_str(&text).expect("count output is JSON");
        assert_eq!(parsed["count"], 0, "validate must not persist a document");
    }

    fn make_media_upload_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("media");
        def.fields = vec![
            FieldDefinition::builder("filename", FieldType::Text)
                .required(true)
                .build(),
            FieldDefinition::builder("url", FieldType::Text).build(),
        ];
        def.upload = Some(CollectionUpload::new());
        def
    }

    /// Regression: an MCP hard-delete on an upload collection must remove the
    /// orphaned file from storage. Before storage was threaded into
    /// `ToolExecCtx`, `exec_delete` passed `None` and left the file behind.
    #[test]
    fn handle_tools_call_delete_cleans_upload_files() {
        let (tmp, server) = make_server_with(vec![make_media_upload_def()]);

        // Place a fake upload file at the storage path the url field points to.
        let media_dir = tmp.path().join("uploads/media");
        std::fs::create_dir_all(&media_dir).unwrap();
        let file = media_dir.join("test.png");
        std::fs::write(&file, b"fake image").unwrap();

        // Create the document referencing the file.
        let req = make_request(
            "tools/call",
            Some(json!(50)),
            Some(json!({
                "name": "create_media",
                "arguments": { "filename": "test.png", "url": "/uploads/media/test.png" }
            })),
        );
        let resp = server.handle_message(req);
        assert!(resp.error.is_none());
        let text = resp.result.unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        let created: Value = serde_json::from_str(&text).expect("create output is JSON");
        let id = created["id"]
            .as_str()
            .expect("created doc has an id")
            .to_string();

        assert!(file.exists(), "upload file should exist before delete");

        // Hard-delete via MCP (media has no soft-delete) → file must be cleaned.
        let req = make_request(
            "tools/call",
            Some(json!(51)),
            Some(json!({
                "name": "delete_media",
                "arguments": { "id": id }
            })),
        );
        let resp = server.handle_message(req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert!(
            result.get("isError").is_none(),
            "delete should succeed: {result}"
        );

        assert!(
            !file.exists(),
            "upload file should be cleaned up after MCP hard-delete"
        );
    }

    #[test]
    fn handle_tools_call_missing_params_returns_error() {
        let (_tmp, server) = make_server();
        let req = make_request("tools/call", Some(json!(8)), None);
        let resp = server.handle_message(req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    #[test]
    fn handle_tools_call_invalid_params_returns_error() {
        let (_tmp, server) = make_server();
        let req = make_request("tools/call", Some(json!(9)), Some(json!("bad")));
        let resp = server.handle_message(req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    #[test]
    fn handle_resources_list_returns_resources() {
        let (_tmp, server) = make_server();
        let req = make_request("resources/list", Some(json!(10)), None);
        let resp = server.handle_message(req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let resources = result["resources"].as_array().unwrap();
        assert!(!resources.is_empty());
        let uris: Vec<&str> = resources
            .iter()
            .map(|r| r["uri"].as_str().unwrap_or(""))
            .collect();
        assert!(uris.contains(&"crap://schema/collections"));
    }

    #[test]
    fn handle_resources_read_collections_schema() {
        let (_tmp, server) = make_server();
        let req = make_request(
            "resources/read",
            Some(json!(11)),
            Some(json!({
                "uri": "crap://schema/collections"
            })),
        );
        let resp = server.handle_message(req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let contents = result["contents"].as_array().unwrap();
        assert!(!contents.is_empty());
        assert!(contents[0]["text"].as_str().unwrap().contains("posts"));
    }

    #[test]
    fn handle_resources_read_unknown_uri_returns_error() {
        let (_tmp, server) = make_server();
        let req = make_request(
            "resources/read",
            Some(json!(12)),
            Some(json!({
                "uri": "crap://nonexistent"
            })),
        );
        let resp = server.handle_message(req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INTERNAL_ERROR);
    }

    #[test]
    fn handle_resources_read_missing_params_returns_error() {
        let (_tmp, server) = make_server();
        let req = make_request("resources/read", Some(json!(13)), None);
        let resp = server.handle_message(req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    #[test]
    fn handle_resources_read_invalid_params_returns_error() {
        let (_tmp, server) = make_server();
        let req = make_request("resources/read", Some(json!(14)), Some(json!("bad")));
        let resp = server.handle_message(req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, INVALID_PARAMS);
    }

    #[test]
    fn handle_tools_call_list_field_types() {
        let (_tmp, server) = make_server();
        let req = make_request(
            "tools/call",
            Some(json!(15)),
            Some(json!({
                "name": "list_field_types",
                "arguments": {}
            })),
        );
        let resp = server.handle_message(req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("text"));
        assert!(text.contains("richtext"));
    }

    #[test]
    fn handle_tools_call_cli_reference() {
        let (_tmp, server) = make_server();
        let req = make_request(
            "tools/call",
            Some(json!(16)),
            Some(json!({
                "name": "cli_reference",
                "arguments": {}
            })),
        );
        let resp = server.handle_message(req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("crap-cms"));
    }

    #[test]
    fn handle_tools_call_describe_collection() {
        let (_tmp, server) = make_server();
        let req = make_request(
            "tools/call",
            Some(json!(17)),
            Some(json!({
                "name": "describe_collection",
                "arguments": { "slug": "posts" }
            })),
        );
        let resp = server.handle_message(req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("posts"));
        assert!(text.contains("collection"));
    }
}
