//! MCP tool generation from Registry and tool execution.
//!
//! **Security model:** MCP operates with full access — no collection-level or field-level
//! access control is applied. This is intentional: MCP is a programmatic API surface
//! (like Lua's `overrideAccess = true`) gated by transport-level auth (API key for HTTP,
//! process-level access for stdio). Access control Lua functions are designed for per-user
//! restrictions and don't apply to machine-to-machine access.

mod crud_tools;
mod static_tools;

use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::config::McpConfig;
use crate::core::Registry;
use crate::db::DbPool;
use crate::hooks::lifecycle::HookRunner;

use super::protocol::ToolDefinition;
use super::schema::{collection_input_schema, global_input_schema, CrudOp};

use crud_tools::*;
use static_tools::*;

/// Parsed tool name: operation + target slug.
#[derive(Debug, PartialEq)]
pub struct ParsedTool {
    pub op: ToolOp,
    pub slug: String,
}

/// Tool operation type.
#[derive(Debug, PartialEq)]
pub enum ToolOp {
    Find,
    FindById,
    Create,
    Update,
    Delete,
    /// Read a global (same as find_by_id but for globals)
    ReadGlobal,
    /// Update a global
    UpdateGlobal,
}

/// Check if a collection should be exposed via MCP.
pub fn should_include(slug: &str, config: &McpConfig) -> bool {
    if config.exclude_collections.contains(&slug.to_string()) {
        return false;
    }
    if config.include_collections.is_empty() {
        return true;
    }
    config.include_collections.contains(&slug.to_string())
}

/// Generate all MCP tool definitions from the registry.
pub fn generate_tools(registry: &Registry, config: &McpConfig) -> Vec<ToolDefinition> {
    let mut tools = Vec::new();

    // Collection CRUD tools
    for (slug, def) in &registry.collections {
        if !should_include(slug, config) {
            continue;
        }

        let label = def.display_name();
        let base_desc = def
            .mcp
            .description
            .as_deref()
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("CRUD operations on {}", label));

        // find_<slug>
        tools.push(ToolDefinition {
            name: format!("find_{}", slug),
            description: Some(format!("Query {} documents. {}", label, base_desc)),
            input_schema: collection_input_schema(def, CrudOp::Find),
        });

        // find_by_id_<slug>
        tools.push(ToolDefinition {
            name: format!("find_by_id_{}", slug),
            description: Some(format!("Get a single {} document by ID", label)),
            input_schema: collection_input_schema(def, CrudOp::FindById),
        });

        // create_<slug>
        tools.push(ToolDefinition {
            name: format!("create_{}", slug),
            description: Some(format!("Create a new {} document", label)),
            input_schema: collection_input_schema(def, CrudOp::Create),
        });

        // update_<slug>
        tools.push(ToolDefinition {
            name: format!("update_{}", slug),
            description: Some(format!("Update an existing {} document", label)),
            input_schema: collection_input_schema(def, CrudOp::Update),
        });

        // delete_<slug>
        tools.push(ToolDefinition {
            name: format!("delete_{}", slug),
            description: Some(format!("Delete a {} document by ID", label)),
            input_schema: collection_input_schema(def, CrudOp::Delete),
        });
    }

    // Global CRUD tools (prefixed with "global_" to avoid collision with collection tools)
    for (slug, def) in &registry.globals {
        let label = def.display_name();

        // global_read_<slug>
        tools.push(ToolDefinition {
            name: format!("global_read_{}", slug),
            description: Some(format!("Read the {} global document", label)),
            input_schema: global_input_schema(def, CrudOp::Find),
        });

        // global_update_<slug>
        tools.push(ToolDefinition {
            name: format!("global_update_{}", slug),
            description: Some(format!("Update the {} global document", label)),
            input_schema: global_input_schema(def, CrudOp::Update),
        });
    }

    // Schema introspection tools
    tools.push(ToolDefinition {
        name: "list_collections".to_string(),
        description: Some("List all collections with their labels and capabilities".to_string()),
        input_schema: json!({ "type": "object", "properties": {} }),
    });

    tools.push(ToolDefinition {
        name: "describe_collection".to_string(),
        description: Some("Get the full field schema for a collection or global".to_string()),
        input_schema: json!({
            "type": "object",
            "properties": {
                "slug": { "type": "string", "description": "Collection or global slug" }
            },
            "required": ["slug"]
        }),
    });

    tools.push(ToolDefinition {
        name: "list_field_types".to_string(),
        description: Some(
            "List all available field types with descriptions and valid options".to_string(),
        ),
        input_schema: json!({ "type": "object", "properties": {} }),
    });

    tools.push(ToolDefinition {
        name: "cli_reference".to_string(),
        description: Some("Get CLI command reference for crap-cms. Returns usage, flags, and examples for all commands or a specific command.".to_string()),
        input_schema: json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Specific command to get help for (e.g., 'serve', 'migrate', 'user create'). Omit for full reference."
                }
            }
        }),
    });

    // Config generation tools (opt-in)
    if config.config_tools {
        tools.push(ToolDefinition {
            name: "read_config_file".to_string(),
            description: Some("Read a file from the config directory".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path within the config directory" }
                },
                "required": ["path"]
            }),
        });

        tools.push(ToolDefinition {
            name: "write_config_file".to_string(),
            description: Some("Write a file to the config directory (creates parent dirs)".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path within the config directory" },
                    "content": { "type": "string", "description": "File content to write" }
                },
                "required": ["path", "content"]
            }),
        });

        tools.push(ToolDefinition {
            name: "list_config_files".to_string(),
            description: Some("List files in the config directory".to_string()),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Subdirectory to list (default: root)" }
                }
            }),
        });
    }

    tools
}

/// Parse a tool name like "find_posts" into (op, slug).
pub fn parse_tool_name(name: &str, registry: &Registry) -> Option<ParsedTool> {
    // Try collection CRUD patterns
    for prefix in &["find_by_id_", "find_", "create_", "update_", "delete_"] {
        if let Some(slug) = name.strip_prefix(prefix) {
            if registry.collections.contains_key(slug) {
                let op = match *prefix {
                    "find_" => ToolOp::Find,
                    "find_by_id_" => ToolOp::FindById,
                    "create_" => ToolOp::Create,
                    "update_" => ToolOp::Update,
                    "delete_" => ToolOp::Delete,
                    _ => unreachable!(),
                };
                return Some(ParsedTool {
                    op,
                    slug: slug.to_string(),
                });
            }
        }
    }

    // Try global patterns (global_read_<slug>, global_update_<slug>)
    for prefix in &["global_read_", "global_update_"] {
        if let Some(slug) = name.strip_prefix(prefix) {
            if registry.globals.contains_key(slug) {
                let op = match *prefix {
                    "global_read_" => ToolOp::ReadGlobal,
                    "global_update_" => ToolOp::UpdateGlobal,
                    _ => unreachable!(),
                };
                return Some(ParsedTool {
                    op,
                    slug: slug.to_string(),
                });
            }
        }
    }

    None
}

/// Execute a tool call and return the result as JSON text.
pub fn execute_tool(
    name: &str,
    args: &Value,
    pool: &DbPool,
    registry: &Arc<Registry>,
    runner: &HookRunner,
    config_dir: &Path,
    config: &crate::config::CrapConfig,
) -> Result<String> {
    // Static tools first
    match name {
        "list_collections" => return exec_list_collections(registry, &config.mcp),
        "describe_collection" => return exec_describe_collection(args, registry, &config.mcp),
        "list_field_types" => return exec_list_field_types(),
        "cli_reference" => return exec_cli_reference(args),
        "read_config_file" | "write_config_file" | "list_config_files" => {
            if !config.mcp.config_tools {
                bail!("Config tools are not enabled. Set config_tools = true in [mcp] config.");
            }
            return match name {
                "read_config_file" => exec_read_config_file(args, config_dir),
                "write_config_file" => exec_write_config_file(args, config_dir),
                "list_config_files" => exec_list_config_files(args, config_dir),
                _ => unreachable!(),
            };
        }
        _ => {}
    }

    // Dynamic CRUD tools
    if let Some(parsed) = parse_tool_name(name, registry) {
        return match parsed.op {
            ToolOp::Find => exec_find(args, &parsed.slug, registry, pool, runner, config),
            ToolOp::FindById => exec_find_by_id(args, &parsed.slug, registry, pool, config),
            ToolOp::Create => exec_create(args, &parsed.slug, registry, pool, runner, config),
            ToolOp::Update => exec_update(args, &parsed.slug, registry, pool, runner, config),
            ToolOp::Delete => exec_delete(args, &parsed.slug, registry, pool, runner),
            ToolOp::ReadGlobal => exec_read_global(&parsed.slug, registry, pool),
            ToolOp::UpdateGlobal => exec_update_global(args, &parsed.slug, registry, pool, runner),
        };
    }

    bail!("Unknown tool: {}", name)
}

#[cfg(test)]
mod tests;
