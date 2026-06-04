//! MCP-specific configuration for a collection or global.

use std::collections::HashMap;

use crate::typegen::lua::LuaAnnotation;
use serde::{Deserialize, Serialize};

/// Operation keys accepted in a collection's `mcp.operations` table. Mirrors
/// the `CrudOp` variants surfaced as collection tools (see
/// `crate::mcp::tools::dispatch`); a guard test there asserts they stay in sync.
pub const COLLECTION_OPERATIONS: &[&str] = &[
    "create",
    "create_many",
    "update",
    "update_many",
    "validate",
    "find",
    "find_by_id",
    "delete",
    "delete_many",
    "undelete",
    "unpublish",
    "count",
    "list_versions",
    "restore_version",
];

/// Operation keys accepted in a global's `mcp.operations` table — globals
/// surface only read/update/validate tools.
pub const GLOBAL_OPERATIONS: &[&str] = &["read", "update", "validate"];

/// MCP-specific configuration for a collection or global.
#[derive(Debug, Clone, Serialize, Deserialize, Default, LuaAnnotation)]
#[serde(default)]
#[lua(class = "crap.McpCollectionConfig")]
pub struct McpConfig {
    /// Description used in MCP tool descriptions for this collection/global.
    pub description: Option<String>,
    /// Per-operation description overrides, keyed by operation name (e.g.
    /// `"create"`, `"delete"`, `"find"`; for globals `"read"` / `"update"` /
    /// `"validate"`). Overrides the auto-generated description for that tool.
    #[serde(default)]
    #[lua(ty = "table<string, string>", optional)]
    pub operations: HashMap<String, String>,
}

impl McpConfig {
    /// Create a new MCP configuration with the given description.
    #[must_use]
    pub fn new(description: Option<String>) -> Self {
        Self {
            description,
            operations: HashMap::new(),
        }
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
        assert!(c.operations.is_empty());
    }

    #[test]
    fn operations_deserialize_as_op_to_description_map() {
        let c: McpConfig =
            serde_json::from_value(json!({ "operations": { "delete": "Purges permanently" } }))
                .unwrap();
        assert_eq!(
            c.operations.get("delete").map(String::as_str),
            Some("Purges permanently")
        );
    }
}
