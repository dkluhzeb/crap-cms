//! MCP tool generation from Registry and tool execution.
//!
//! **Security model:** MCP operates with full access — no collection-level or field-level
//! access control is applied. This is intentional: MCP is a programmatic API surface
//! (like Lua's `overrideAccess = true`) gated by transport-level auth (API key for HTTP,
//! process-level access for stdio). Access control Lua functions are designed for per-user
//! restrictions and don't apply to machine-to-machine access.

use std::{path::Path, sync::Arc};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::{
    config::McpConfig,
    core::{
        Registry,
        cache::SharedCache,
        event::{SharedEventTransport, SharedInvalidationTransport},
    },
    db::DbPool,
    hooks::HookRunner,
    mcp::{
        protocol::ToolDefinition,
        schema::{CrudOp, collection_input_schema, global_input_schema},
    },
};

use super::{
    collection::{
        read::{exec_count, exec_find, exec_find_by_id},
        versions::{exec_list_versions, exec_restore_version},
        write::{
            UnpublishParams, exec_create, exec_create_many, exec_delete, exec_delete_many,
            exec_undelete, exec_unpublish, exec_update, exec_update_many,
        },
    },
    globals::{exec_read_global, exec_update_global},
    schema::{
        exec_cli_reference, exec_describe_collection, exec_list_collections,
        exec_list_config_files, exec_list_field_types, exec_read_config_file,
        exec_write_config_file,
    },
};

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
    Count,
    Create,
    CreateMany,
    Update,
    UpdateMany,
    Delete,
    DeleteMany,
    Undelete,
    Unpublish,
    ListVersions,
    RestoreVersion,
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

        // create_many_<slug>
        tools.push(ToolDefinition {
            name: format!("create_many_{}", slug),
            description: Some(format!(
                "Bulk create multiple {} documents in batched transactions",
                label
            )),
            input_schema: collection_input_schema(def, CrudOp::CreateMany),
        });

        // update_many_<slug>
        tools.push(ToolDefinition {
            name: format!("update_many_{}", slug),
            description: Some(format!(
                "Bulk update multiple {} documents matching a filter",
                label
            )),
            input_schema: collection_input_schema(def, CrudOp::UpdateMany),
        });

        // delete_many_<slug>
        tools.push(ToolDefinition {
            name: format!("delete_many_{}", slug),
            description: Some(format!(
                "Bulk delete multiple {} documents matching a filter",
                label
            )),
            input_schema: collection_input_schema(def, CrudOp::DeleteMany),
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

        // count_<slug>
        tools.push(ToolDefinition {
            name: format!("count_{}", slug),
            description: Some(format!("Count {} documents matching filters", label)),
            input_schema: collection_input_schema(def, CrudOp::Count),
        });

        // undelete_<slug> — only for collections with soft delete
        if def.has_soft_delete() {
            tools.push(ToolDefinition {
                name: format!("undelete_{}", slug),
                description: Some(format!("Restore a soft-deleted {} document", label)),
                input_schema: collection_input_schema(def, CrudOp::Undelete),
            });
        }

        // unpublish_<slug> — only for versioned collections
        if def.versions.is_some() {
            tools.push(ToolDefinition {
                name: format!("unpublish_{}", slug),
                description: Some(format!("Unpublish a {} document (set to draft)", label)),
                input_schema: collection_input_schema(def, CrudOp::Unpublish),
            });

            // list_versions_<slug>
            tools.push(ToolDefinition {
                name: format!("list_versions_{}", slug),
                description: Some(format!("List version history for a {} document", label)),
                input_schema: collection_input_schema(def, CrudOp::ListVersions),
            });

            // restore_version_<slug>
            tools.push(ToolDefinition {
                name: format!("restore_version_{}", slug),
                description: Some(format!(
                    "Restore a {} document to a specific version",
                    label
                )),
                input_schema: collection_input_schema(def, CrudOp::RestoreVersion),
            });
        }
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
    // Try collection CRUD patterns (longer prefixes first to avoid ambiguity)
    for prefix in &[
        "find_by_id_",
        "find_",
        "count_",
        "create_many_",
        "create_",
        "update_many_",
        "update_",
        "delete_many_",
        "delete_",
        "undelete_",
        "unpublish_",
        "list_versions_",
        "restore_version_",
    ] {
        if let Some(slug) = name.strip_prefix(prefix)
            && registry.collections.contains_key(slug)
        {
            let op = match *prefix {
                "find_" => ToolOp::Find,
                "find_by_id_" => ToolOp::FindById,
                "count_" => ToolOp::Count,
                "create_many_" => ToolOp::CreateMany,
                "create_" => ToolOp::Create,
                "update_many_" => ToolOp::UpdateMany,
                "update_" => ToolOp::Update,
                "delete_many_" => ToolOp::DeleteMany,
                "delete_" => ToolOp::Delete,
                "undelete_" => ToolOp::Undelete,
                "unpublish_" => ToolOp::Unpublish,
                "list_versions_" => ToolOp::ListVersions,
                "restore_version_" => ToolOp::RestoreVersion,
                _ => unreachable!(),
            };

            return Some(ParsedTool {
                op,
                slug: slug.to_string(),
            });
        }
    }

    // Try global patterns (global_read_<slug>, global_update_<slug>)
    for prefix in &["global_read_", "global_update_"] {
        if let Some(slug) = name.strip_prefix(prefix)
            && registry.globals.contains_key(slug)
        {
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

    None
}

/// Execute a tool call and return the result as JSON text.
#[allow(clippy::too_many_arguments)]
pub fn execute_tool(
    name: &str,
    args: &Value,
    pool: &DbPool,
    registry: &Arc<Registry>,
    runner: &HookRunner,
    config_dir: &Path,
    config: &crate::config::CrapConfig,
    event_transport: Option<SharedEventTransport>,
    invalidation_transport: Option<SharedInvalidationTransport>,
    cache: Option<SharedCache>,
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
        // Enforce include/exclude at execution time — not just in tools/list.
        // Without this, an attacker who knows a collection slug could directly call
        // e.g. find_<slug> even if the collection was excluded from tool listing.
        if !should_include(&parsed.slug, &config.mcp) {
            bail!("Tool not available: {}", name);
        }

        return match parsed.op {
            ToolOp::Find => exec_find(args, &parsed.slug, registry, pool, runner, config),
            ToolOp::FindById => exec_find_by_id(args, &parsed.slug, registry, pool, runner, config),
            ToolOp::Count => exec_count(args, &parsed.slug, registry, pool, runner),
            ToolOp::Create => exec_create(
                args,
                &parsed.slug,
                registry,
                pool,
                runner,
                config,
                event_transport,
                cache,
            ),
            ToolOp::CreateMany => exec_create_many(
                args,
                &parsed.slug,
                registry,
                pool,
                runner,
                event_transport,
                cache,
            ),
            ToolOp::Update => exec_update(
                args,
                &parsed.slug,
                registry,
                pool,
                runner,
                config,
                event_transport,
                cache,
            ),
            ToolOp::UpdateMany => exec_update_many(
                args,
                &parsed.slug,
                registry,
                pool,
                runner,
                config,
                event_transport,
                cache,
            ),
            ToolOp::Delete => exec_delete(
                args,
                &parsed.slug,
                registry,
                pool,
                runner,
                event_transport,
                invalidation_transport,
                cache,
            ),
            ToolOp::DeleteMany => exec_delete_many(
                args,
                &parsed.slug,
                registry,
                pool,
                runner,
                config,
                event_transport,
                invalidation_transport,
                cache,
            ),
            ToolOp::Undelete => exec_undelete(
                args,
                &parsed.slug,
                registry,
                pool,
                runner,
                event_transport,
                cache,
            ),
            ToolOp::Unpublish => exec_unpublish(UnpublishParams {
                args,
                slug: &parsed.slug,
                registry,
                pool,
                runner,
                config,
                event_transport,
                cache,
            }),
            ToolOp::ListVersions => exec_list_versions(args, &parsed.slug, registry, pool, runner),
            ToolOp::RestoreVersion => exec_restore_version(
                args,
                &parsed.slug,
                registry,
                pool,
                runner,
                config,
                event_transport,
                cache,
            ),
            ToolOp::ReadGlobal => exec_read_global(&parsed.slug, registry, pool, runner),
            ToolOp::UpdateGlobal => exec_update_global(
                args,
                &parsed.slug,
                registry,
                pool,
                runner,
                event_transport,
                cache,
            ),
        };
    }

    bail!("Unknown tool: {}", name)
}
