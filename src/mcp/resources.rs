//! MCP resource definitions and handlers.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, to_string_pretty, to_value};
use tracing::error;

use crate::mcp::{
    access::McpExposure,
    protocol::{ResourceContent, ResourceDefinition},
    schema::{CrudOp, collection_input_schema, global_input_schema},
    tools::should_include,
};
use crate::{config::CrapConfig, core::Registry};

/// Per-collection metadata + schema, keyed by slug in
/// [`collections_schema`]'s output. The `schema` field is JSON Schema —
/// stays as `Value` because that's its native shape.
#[derive(Serialize)]
struct CollectionSchemaEntry {
    label: String,
    timestamps: bool,
    has_auth: bool,
    has_upload: bool,
    has_drafts: bool,
    schema: Value,
}

/// Per-global metadata + schema, keyed by slug in [`globals_schema`].
#[derive(Serialize)]
struct GlobalSchemaEntry {
    label: String,
    schema: Value,
}

/// List all available MCP resources.
pub(in crate::mcp) fn list_resources() -> Vec<ResourceDefinition> {
    vec![
        ResourceDefinition {
            uri: "crap://schema/collections".to_string(),
            name: "Collection Schemas".to_string(),
            description: Some("Full schema of all collections as JSON".to_string()),
            mime_type: Some("application/json".to_string()),
        },
        ResourceDefinition {
            uri: "crap://schema/globals".to_string(),
            name: "Global Schemas".to_string(),
            description: Some("Full schema of all globals as JSON".to_string()),
            mime_type: Some("application/json".to_string()),
        },
        ResourceDefinition {
            uri: "crap://config".to_string(),
            name: "Configuration".to_string(),
            description: Some("Current crap.toml configuration (sanitized)".to_string()),
            mime_type: Some("application/json".to_string()),
        },
    ]
}

/// Serialize a value to pretty JSON, logging and returning `"{}"` on failure.
fn serialize_pretty(value: &impl Serialize, label: &str) -> String {
    match to_string_pretty(value) {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to serialize MCP {label}: {e}");
            "{}".to_string()
        }
    }
}

/// Build a [`ResourceContent`] with `application/json` mime type.
fn json_resource(uri: &str, text: String) -> ResourceContent {
    ResourceContent {
        uri: uri.to_string(),
        mime_type: Some("application/json".to_string()),
        text,
    }
}

/// Build the schema map for all visible collections.
fn collections_schema(
    registry: &Registry,
    config: &CrapConfig,
    exposure: &McpExposure,
) -> BTreeMap<String, CollectionSchemaEntry> {
    let mut schemas = BTreeMap::new();

    for (slug, def) in &registry.collections {
        if !should_include(slug, &config.mcp) || !exposure.allows(slug) {
            continue;
        }

        schemas.insert(
            slug.to_string(),
            CollectionSchemaEntry {
                label: def.display_name().to_string(),
                timestamps: def.timestamps,
                has_auth: def.is_auth_collection(),
                has_upload: def.is_upload_collection(),
                has_drafts: def.has_drafts(),
                schema: collection_input_schema(def, CrudOp::Create),
            },
        );
    }

    schemas
}

/// Build the schema map for all globals.
fn globals_schema(
    registry: &Registry,
    config: &CrapConfig,
    exposure: &McpExposure,
) -> BTreeMap<String, GlobalSchemaEntry> {
    let mut schemas = BTreeMap::new();

    for (slug, def) in &registry.globals {
        // Mirrors the tool listing + execution filter: config include/exclude
        // lists apply to globals by slug as well as `access.mcp`.
        if !should_include(slug, &config.mcp) || !exposure.allows(slug) {
            continue;
        }
        schemas.insert(
            slug.to_string(),
            GlobalSchemaEntry {
                label: def.display_name().to_string(),
                schema: global_input_schema(def, CrudOp::Update),
            },
        );
    }

    schemas
}

/// Serialize the sanitized config to pretty JSON.
fn config_resource(config: &CrapConfig) -> String {
    // auth.secret, email.smtp_pass, and mcp.api_key are auto-redacted via Serialize impls
    let config_json = to_value(config).unwrap_or(Value::Null);
    serialize_pretty(&config_json, "config")
}

/// Read a resource by URI.
pub(in crate::mcp) fn read_resource(
    uri: &str,
    registry: &Registry,
    config: &CrapConfig,
    exposure: &McpExposure,
) -> Option<ResourceContent> {
    match uri {
        "crap://schema/collections" => {
            let schemas = collections_schema(registry, config, exposure);
            Some(json_resource(
                uri,
                serialize_pretty(&schemas, "collection schemas"),
            ))
        }
        "crap://schema/globals" => {
            let schemas = globals_schema(registry, config, exposure);
            Some(json_resource(
                uri,
                serialize_pretty(&schemas, "global schemas"),
            ))
        }
        "crap://config" => Some(json_resource(uri, config_resource(config))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CollectionDefinition, GlobalDefinition, JwtSecret};

    #[test]
    fn list_resources_returns_three() {
        let resources = list_resources();
        assert_eq!(resources.len(), 3);
        assert!(
            resources
                .iter()
                .any(|r| r.uri == "crap://schema/collections")
        );
        assert!(resources.iter().any(|r| r.uri == "crap://schema/globals"));
        assert!(resources.iter().any(|r| r.uri == "crap://config"));
    }

    #[test]
    fn read_collections_resource() {
        let mut reg = Registry::new();
        reg.register_collection(CollectionDefinition::new("posts"));
        let config = CrapConfig::default();
        let content = read_resource(
            "crap://schema/collections",
            &reg,
            &config,
            &McpExposure::default(),
        )
        .unwrap();
        assert!(content.text.contains("posts"));
    }

    #[test]
    fn read_globals_resource() {
        let mut reg = Registry::new();
        reg.register_global(GlobalDefinition::new("settings"));
        let config = CrapConfig::default();
        let content = read_resource(
            "crap://schema/globals",
            &reg,
            &config,
            &McpExposure::default(),
        )
        .unwrap();
        assert!(content.text.contains("settings"));
    }

    #[test]
    fn read_config_sanitizes_secrets() {
        let reg = Registry::new();
        let mut config = CrapConfig::default();
        config.auth.secret = JwtSecret::new("super-secret");
        let content =
            read_resource("crap://config", &reg, &config, &McpExposure::default()).unwrap();
        assert!(!content.text.contains("super-secret"));
        assert!(content.text.contains("[REDACTED]"));
    }

    #[test]
    fn read_unknown_resource() {
        let reg = Registry::new();
        let config = CrapConfig::default();
        assert!(read_resource("crap://unknown", &reg, &config, &McpExposure::default()).is_none());
    }
}
