//! MCP-specific configuration for a collection or global.

use serde::{Deserialize, Serialize};

/// MCP-specific configuration for a collection or global.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct McpConfig {
    /// Description used in MCP tool descriptions for this collection/global.
    pub description: Option<String>,
}

impl McpConfig {
    /// Create a new MCP configuration with the given description.
    pub fn new(description: Option<String>) -> Self {
        Self { description }
    }
}
