//! MCP resource definitions and handlers.

use serde_json::{Map, Value, json};
use tracing::error;

use crate::mcp::{
    protocol::{ResourceContent, ResourceDefinition},
    schema::{CrudOp, collection_input_schema, global_input_schema},
    tools::should_include,
};
use crate::{config::CrapConfig, core::Registry};

/// List all available MCP resources.
pub fn list_resources() -> Vec<ResourceDefinition> {
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

/// Read a resource by URI.
pub fn read_resource(
    uri: &str,
    registry: &Registry,
    config: &CrapConfig,
) -> Option<ResourceContent> {
    match uri {
        "crap://schema/collections" => {
            let mut schemas = Map::new();
            for (slug, def) in &registry.collections {
                if !should_include(slug, &config.mcp) {
                    continue;
                }
                let schema = collection_input_schema(def, CrudOp::Create);
                schemas.insert(
                    slug.to_string(),
                    json!({
                        "label": def.display_name(),
                        "timestamps": def.timestamps,
                        "has_auth": def.is_auth_collection(),
                        "has_upload": def.is_upload_collection(),
                        "has_drafts": def.has_drafts(),
                        "schema": schema,
                    }),
                );
            }
            Some(ResourceContent {
                uri: uri.to_string(),
                mime_type: Some("application/json".to_string()),
                text: match serde_json::to_string_pretty(&schemas) {
                    Ok(s) => s,
                    Err(e) => {
                        error!("Failed to serialize MCP collection schemas: {}", e);
                        "{}".to_string()
                    }
                },
            })
        }
        "crap://schema/globals" => {
            let mut schemas = Map::new();
            for (slug, def) in &registry.globals {
                let schema = global_input_schema(def, CrudOp::Update);
                schemas.insert(
                    slug.to_string(),
                    json!({
                        "label": def.display_name(),
                        "schema": schema,
                    }),
                );
            }
            Some(ResourceContent {
                uri: uri.to_string(),
                mime_type: Some("application/json".to_string()),
                text: match serde_json::to_string_pretty(&schemas) {
                    Ok(s) => s,
                    Err(e) => {
                        error!("Failed to serialize MCP global schemas: {}", e);
                        "{}".to_string()
                    }
                },
            })
        }
        "crap://config" => {
            // Sanitize config: redact secrets
            let config_json = serde_json::to_value(config).unwrap_or(Value::Null);

            // auth.secret, email.smtp_pass, and mcp.api_key are auto-redacted via Serialize impls
            Some(ResourceContent {
                uri: uri.to_string(),
                mime_type: Some("application/json".to_string()),
                text: match serde_json::to_string_pretty(&config_json) {
                    Ok(s) => s,
                    Err(e) => {
                        error!("Failed to serialize MCP config: {}", e);
                        "{}".to_string()
                    }
                },
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::{CollectionDefinition, GlobalDefinition};

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
        let content = read_resource("crap://schema/collections", &reg, &config).unwrap();
        assert!(content.text.contains("posts"));
    }

    #[test]
    fn read_globals_resource() {
        let mut reg = Registry::new();
        reg.register_global(GlobalDefinition::new("settings"));
        let config = CrapConfig::default();
        let content = read_resource("crap://schema/globals", &reg, &config).unwrap();
        assert!(content.text.contains("settings"));
    }

    #[test]
    fn read_config_sanitizes_secrets() {
        let reg = Registry::new();
        let mut config = CrapConfig::default();
        config.auth.secret = crate::core::JwtSecret::new("super-secret");
        let content = read_resource("crap://config", &reg, &config).unwrap();
        assert!(!content.text.contains("super-secret"));
        assert!(content.text.contains("[REDACTED]"));
    }

    #[test]
    fn read_unknown_resource() {
        let reg = Registry::new();
        let config = CrapConfig::default();
        assert!(read_resource("crap://unknown", &reg, &config).is_none());
    }
}
