//! MCP resource definitions and handlers.

use serde_json::{json, Value};

use crate::config::CrapConfig;
use crate::core::Registry;
use super::protocol::{ResourceDefinition, ResourceContent};
use super::schema::{CrudOp, collection_input_schema, global_input_schema};

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
pub fn read_resource(uri: &str, registry: &Registry, config: &CrapConfig) -> Option<ResourceContent> {
    match uri {
        "crap://schema/collections" => {
            let mut schemas = serde_json::Map::new();
            for (slug, def) in &registry.collections {
                let schema = collection_input_schema(def, CrudOp::Create);
                schemas.insert(slug.clone(), json!({
                    "label": def.display_name(),
                    "timestamps": def.timestamps,
                    "has_auth": def.is_auth_collection(),
                    "has_upload": def.is_upload_collection(),
                    "has_drafts": def.has_drafts(),
                    "schema": schema,
                }));
            }
            Some(ResourceContent {
                uri: uri.to_string(),
                mime_type: Some("application/json".to_string()),
                text: serde_json::to_string_pretty(&schemas).unwrap_or_default(),
            })
        }
        "crap://schema/globals" => {
            let mut schemas = serde_json::Map::new();
            for (slug, def) in &registry.globals {
                let schema = global_input_schema(def, CrudOp::Update);
                schemas.insert(slug.clone(), json!({
                    "label": def.display_name(),
                    "schema": schema,
                }));
            }
            Some(ResourceContent {
                uri: uri.to_string(),
                mime_type: Some("application/json".to_string()),
                text: serde_json::to_string_pretty(&schemas).unwrap_or_default(),
            })
        }
        "crap://config" => {
            // Sanitize config: redact secrets
            let mut config_json = serde_json::to_value(config).unwrap_or(Value::Null);
            if let Some(obj) = config_json.as_object_mut() {
                // Redact auth secret
                if let Some(auth) = obj.get_mut("auth").and_then(|a| a.as_object_mut()) {
                    if auth.contains_key("secret") {
                        auth.insert("secret".to_string(), Value::String("***".to_string()));
                    }
                }
                // Redact email password
                if let Some(email) = obj.get_mut("email").and_then(|e| e.as_object_mut()) {
                    if email.contains_key("smtp_pass") {
                        email.insert("smtp_pass".to_string(), Value::String("***".to_string()));
                    }
                }
                // Redact MCP API key
                if let Some(mcp) = obj.get_mut("mcp").and_then(|m| m.as_object_mut()) {
                    if let Some(key) = mcp.get("api_key").and_then(|k| k.as_str()) {
                        if !key.is_empty() {
                            mcp.insert("api_key".to_string(), Value::String("***".to_string()));
                        }
                    }
                }
            }
            Some(ResourceContent {
                uri: uri.to_string(),
                mime_type: Some("application/json".to_string()),
                text: serde_json::to_string_pretty(&config_json).unwrap_or_default(),
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
        assert!(resources.iter().any(|r| r.uri == "crap://schema/collections"));
        assert!(resources.iter().any(|r| r.uri == "crap://schema/globals"));
        assert!(resources.iter().any(|r| r.uri == "crap://config"));
    }

    #[test]
    fn read_collections_resource() {
        let mut reg = Registry::new();
        reg.register_collection(CollectionDefinition {
            slug: "posts".to_string(),
            ..Default::default()
        });
        let config = CrapConfig::default();
        let content = read_resource("crap://schema/collections", &reg, &config).unwrap();
        assert!(content.text.contains("posts"));
    }

    #[test]
    fn read_globals_resource() {
        let mut reg = Registry::new();
        reg.register_global(GlobalDefinition {
            slug: "settings".to_string(),
            ..Default::default()
        });
        let config = CrapConfig::default();
        let content = read_resource("crap://schema/globals", &reg, &config).unwrap();
        assert!(content.text.contains("settings"));
    }

    #[test]
    fn read_config_sanitizes_secrets() {
        let reg = Registry::new();
        let mut config = CrapConfig::default();
        config.auth.secret = "super-secret".to_string();
        let content = read_resource("crap://config", &reg, &config).unwrap();
        assert!(!content.text.contains("super-secret"));
        assert!(content.text.contains("***"));
    }

    #[test]
    fn read_unknown_resource() {
        let reg = Registry::new();
        let config = CrapConfig::default();
        assert!(read_resource("crap://unknown", &reg, &config).is_none());
    }
}
