//! MCP (Model Context Protocol) server for Crap CMS.
//!
//! Provides auto-generated tool definitions from the Lua-defined Registry,
//! schema introspection resources, and optional config generation tools.
//! Supports stdio and HTTP transports.

pub mod protocol;
pub mod resources;
pub mod schema;
pub mod server;
pub mod stdio;
pub mod tools;

pub use server::McpServer;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::CollectionDefinition;
    use crate::db::{migrate, pool};
    use crate::hooks::lifecycle::HookRunner;
    use crate::{config::CrapConfig, core::Registry};
    use protocol::{
        INTERNAL_ERROR, INVALID_PARAMS, InitializeParams, JsonRpcRequest, METHOD_NOT_FOUND,
        PROTOCOL_VERSION,
    };
    use serde_json::{Value, json};

    fn make_request(method: &str, id: Option<Value>, params: Option<Value>) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        }
    }

    /// Build a full McpServer backed by a real SQLite pool and HookRunner.
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

        migrate::sync_all(&db_pool, &shared, &config.locale).expect("sync schema");

        let registry = Registry::snapshot(&shared);
        let runner = HookRunner::builder()
            .config_dir(tmp.path())
            .registry(shared)
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
