//! Model Context Protocol (MCP) server configuration.

use serde::{Deserialize, Serialize};

use crate::config::McpApiKey;

/// MCP (Model Context Protocol) server configuration.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
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
}
