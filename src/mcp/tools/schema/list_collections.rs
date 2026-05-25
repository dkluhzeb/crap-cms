//! `list_collections` MCP tool — enumerate collections and globals with metadata.

use anyhow::Result;
use serde::Serialize;
use serde_json::to_string_pretty;

use crate::{config::McpConfig, core::Registry, mcp::tools::should_include};

/// One entry in the `list_collections` MCP tool response.
///
/// Untagged sum: collection entries omit `type`; global entries set
/// `type: "global"`. Wire-format-compatible with prior releases.
#[derive(Serialize)]
#[serde(untagged)]
enum ListEntry<'a> {
    Collection {
        slug: &'a str,
        label: String,
        fields: usize,
        has_auth: bool,
        has_upload: bool,
        has_drafts: bool,
    },
    Global {
        slug: &'a str,
        label: String,
        #[serde(rename = "type")]
        kind: &'static str,
        fields: usize,
    },
}

/// List all collections and globals with metadata.
pub(in crate::mcp::tools) fn exec_list_collections(
    registry: &Registry,
    mcp_config: &McpConfig,
) -> Result<String> {
    let mut result = Vec::new();
    for (slug, def) in &registry.collections {
        if !should_include(slug, mcp_config) {
            continue;
        }
        result.push(ListEntry::Collection {
            slug,
            label: def.display_name().to_string(),
            fields: def.fields.len(),
            has_auth: def.is_auth_collection(),
            has_upload: def.is_upload_collection(),
            has_drafts: def.has_drafts(),
        });
    }
    for (slug, def) in &registry.globals {
        result.push(ListEntry::Global {
            slug,
            label: def.display_name().to_string(),
            kind: "global",
            fields: def.fields.len(),
        });
    }
    Ok(to_string_pretty(&result)?)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, from_str};

    use super::*;
    use crate::mcp::tools::test_helpers::make_registry;

    #[test]
    fn exec_list_collections_returns_all() {
        let reg = make_registry();
        let config = McpConfig::default();
        let result = exec_list_collections(&reg, &config).unwrap();
        let items: Vec<Value> = from_str(&result).unwrap();
        // posts, users (collections) + settings (global)
        assert!(items.len() >= 3);
        let slugs: Vec<&str> = items
            .iter()
            .map(|i| i["slug"].as_str().unwrap_or(""))
            .collect();
        assert!(slugs.contains(&"posts"));
        assert!(slugs.contains(&"settings"));
    }

    #[test]
    fn exec_list_collections_respects_exclude() {
        let reg = make_registry();
        let config = McpConfig {
            exclude_collections: vec!["users".to_string()],
            ..Default::default()
        };
        let result = exec_list_collections(&reg, &config).unwrap();
        let items: Vec<Value> = from_str(&result).unwrap();
        let slugs: Vec<&str> = items
            .iter()
            .map(|i| i["slug"].as_str().unwrap_or(""))
            .collect();
        assert!(!slugs.contains(&"users"));
        assert!(slugs.contains(&"posts"));
    }

    #[test]
    fn exec_list_collections_empty_registry() {
        let reg = Registry::new();
        let config = McpConfig::default();
        let result = exec_list_collections(&reg, &config).unwrap();
        let items: Vec<Value> = from_str(&result).unwrap();
        assert!(items.is_empty());
    }
}
