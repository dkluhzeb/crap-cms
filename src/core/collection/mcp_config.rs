//! MCP-specific configuration for a collection or global.

use crate::typegen::lua::LuaAnnotation;
use serde::{Deserialize, Serialize};

/// MCP-specific configuration for a collection or global.
#[derive(Debug, Clone, Serialize, Deserialize, Default, LuaAnnotation)]
#[serde(default)]
#[lua(class = "crap.McpCollectionConfig")]
pub struct McpConfig {
    /// Description used in MCP tool descriptions for this collection/global.
    pub description: Option<String>,
}

impl McpConfig {
    /// Create a new MCP configuration with the given description.
    #[must_use]
    pub fn new(description: Option<String>) -> Self {
        Self { description }
    }
}
