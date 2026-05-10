//! `describe_collection` MCP tool — full schema for one collection or global.

use anyhow::{Context as _, Result, bail};
use serde::Serialize;
use serde_json::{Value, to_string_pretty};

use crate::{
    config::McpConfig,
    core::Registry,
    mcp::{
        schema::{CrudOp, collection_input_schema, global_input_schema},
        tools::should_include,
    },
};

/// Response shape for the `describe_collection` MCP tool. Internally
/// tagged on the `type` discriminator (`"collection"` or `"global"`).
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DescribeResponse<'a> {
    Collection {
        slug: &'a str,
        label: String,
        timestamps: bool,
        has_auth: bool,
        has_upload: bool,
        has_drafts: bool,
        schema: Value,
    },
    Global {
        slug: &'a str,
        label: String,
        schema: Value,
    },
}

/// Describe a single collection or global by slug, including its full schema.
pub(in crate::mcp::tools) fn exec_describe_collection(
    args: &Value,
    registry: &Registry,
    mcp_config: &McpConfig,
) -> Result<String> {
    let slug = args
        .get("slug")
        .and_then(|v| v.as_str())
        .context("Missing 'slug' argument")?;

    if let Some(def) = registry.collections.get(slug) {
        if !should_include(slug, mcp_config) {
            bail!("Unknown collection or global: {}", slug);
        }
        let response = DescribeResponse::Collection {
            slug,
            label: def.display_name().to_string(),
            timestamps: def.timestamps,
            has_auth: def.is_auth_collection(),
            has_upload: def.is_upload_collection(),
            has_drafts: def.has_drafts(),
            schema: collection_input_schema(def, CrudOp::Create),
        };

        return Ok(to_string_pretty(&response)?);
    }

    if let Some(def) = registry.globals.get(slug) {
        let response = DescribeResponse::Global {
            slug,
            label: def.display_name().to_string(),
            schema: global_input_schema(def, CrudOp::Update),
        };

        return Ok(to_string_pretty(&response)?);
    }

    bail!("Unknown collection or global: {}", slug)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, from_str, json};

    use super::*;
    use crate::mcp::tools::test_helpers::make_registry;

    #[test]
    fn exec_describe_collection_for_collection() {
        let reg = make_registry();
        let config = McpConfig::default();
        let args = json!({ "slug": "posts" });
        let result = exec_describe_collection(&args, &reg, &config).unwrap();
        let parsed: Value = from_str(&result).unwrap();
        assert_eq!(parsed["slug"], "posts");
        assert_eq!(parsed["type"], "collection");
        assert!(parsed["schema"].is_object());
    }

    #[test]
    fn exec_describe_collection_for_global() {
        let reg = make_registry();
        let config = McpConfig::default();
        let args = json!({ "slug": "settings" });
        let result = exec_describe_collection(&args, &reg, &config).unwrap();
        let parsed: Value = from_str(&result).unwrap();
        assert_eq!(parsed["slug"], "settings");
        assert_eq!(parsed["type"], "global");
        assert!(parsed["schema"].is_object());
    }

    #[test]
    fn exec_describe_collection_unknown_slug_errors() {
        let reg = make_registry();
        let config = McpConfig::default();
        let args = json!({ "slug": "nonexistent" });
        let err = exec_describe_collection(&args, &reg, &config).unwrap_err();
        assert!(err.to_string().contains("Unknown"));
    }

    #[test]
    fn exec_describe_collection_missing_slug_errors() {
        let reg = make_registry();
        let config = McpConfig::default();
        let args = json!({});
        let err = exec_describe_collection(&args, &reg, &config).unwrap_err();
        assert!(err.to_string().contains("slug"));
    }

    #[test]
    fn exec_describe_collection_excluded_errors() {
        let reg = make_registry();
        let config = McpConfig {
            exclude_collections: vec!["posts".to_string()],
            ..Default::default()
        };
        let args = json!({ "slug": "posts" });
        let err = exec_describe_collection(&args, &reg, &config).unwrap_err();
        assert!(err.to_string().contains("Unknown"));
    }
}
