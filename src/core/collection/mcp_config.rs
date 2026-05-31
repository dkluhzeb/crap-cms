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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn new_sets_description_and_serde_defaults_to_none() {
        assert_eq!(
            McpConfig::new(Some("desc".into())).description.as_deref(),
            Some("desc")
        );
        let c: McpConfig = serde_json::from_value(json!({})).unwrap();
        assert!(c.description.is_none());
    }
}
