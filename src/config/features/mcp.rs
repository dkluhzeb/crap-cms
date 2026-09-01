//! Model Context Protocol (MCP) server configuration.

use serde::{Deserialize, Serialize};

use crate::config::{McpApiKey, parsing::serde_filesize};

/// MCP (Model Context Protocol) server configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    /// Enable MCP server (default: false).
    pub enabled: bool,
    /// Enable HTTP transport on /mcp (default: false).
    pub http: bool,
    /// Enable config generation tools that can write files to disk (default: false).
    pub config_tools: bool,
    /// API key for HTTP transport auth. **Required** when `http = true` -- the server
    /// will refuse to start without one. The HTTP handler also rejects all requests
    /// when the API key is empty as a defense-in-depth measure.
    pub api_key: McpApiKey,
    /// Whitelist of collection slugs to expose (empty = all).
    pub include_collections: Vec<String>,
    /// Blacklist of collection slugs to hide (takes precedence over include).
    pub exclude_collections: Vec<String>,
    /// Maximum HTTP request-body size for the `/mcp` endpoint, in bytes.
    /// Accepts integer bytes or a filesize string ("1MB", "16MB").
    /// Default: 1 MiB. Raise it when MCP clients push large payloads
    /// (bulk creates, `write_config_file` with big assets).
    #[serde(with = "serde_filesize")]
    pub http_max_body_bytes: u64,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            http: false,
            config_tools: false,
            api_key: McpApiKey::default(),
            include_collections: Vec::new(),
            exclude_collections: Vec::new(),
            http_max_body_bytes: 1_048_576, // 1 MiB
        }
    }
}
