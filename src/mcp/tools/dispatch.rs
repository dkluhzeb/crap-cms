//! MCP tool generation from Registry and tool execution.
//!
//! **Security model:** MCP operates with full access — no collection-level or field-level
//! access control is applied. This is intentional: MCP is a programmatic API surface
//! (like Lua's `overrideAccess = true`) gated by transport-level auth (API key for HTTP,
//! process-level access for stdio). Access control Lua functions are designed for per-user
//! restrictions and don't apply to machine-to-machine access.

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};

use crate::{
    config::McpConfig,
    core::Registry,
    mcp::{
        protocol::ToolDefinition,
        schema::{CrudOp, collection_input_schema, global_input_schema},
    },
};

use super::{
    ToolExecCtx,
    collection::{
        read::{exec_count, exec_find, exec_find_by_id},
        versions::{exec_list_versions, exec_restore_version},
        write::{
            exec_create, exec_create_many, exec_delete, exec_delete_many, exec_undelete,
            exec_unpublish, exec_update, exec_update_many,
        },
    },
    globals::{exec_read_global, exec_update_global},
    schema::{
        exec_cli_reference, exec_describe_collection, exec_list_collections,
        exec_list_config_files, exec_list_field_types, exec_read_config_file,
        exec_write_config_file,
    },
};

// Static (non-CRUD) tool names. Each appears at exactly two production
// sites — the `generate_tools` declaration and the `execute_tool` match
// arm. Hoisted so a typo at one site can't silently desync them. Tests
// keep the literal so they continue to verify the wire-protocol string.
const TOOL_LIST_COLLECTIONS: &str = "list_collections";
const TOOL_DESCRIBE_COLLECTION: &str = "describe_collection";
const TOOL_LIST_FIELD_TYPES: &str = "list_field_types";
const TOOL_CLI_REFERENCE: &str = "cli_reference";
const TOOL_READ_CONFIG_FILE: &str = "read_config_file";
const TOOL_WRITE_CONFIG_FILE: &str = "write_config_file";
const TOOL_LIST_CONFIG_FILES: &str = "list_config_files";

/// Parsed tool name: operation + target slug.
#[derive(Debug, PartialEq)]
pub(in crate::mcp) struct ParsedTool {
    pub op: ToolOp,
    pub slug: String,
}

/// Tool operation type.
#[derive(Debug, PartialEq)]
pub(in crate::mcp) enum ToolOp {
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
pub(in crate::mcp) fn should_include(slug: &str, config: &McpConfig) -> bool {
    if config.exclude_collections.contains(&slug.to_string()) {
        return false;
    }
    if config.include_collections.is_empty() {
        return true;
    }
    config.include_collections.contains(&slug.to_string())
}

/// Generate all MCP tool definitions from the registry.
pub(in crate::mcp) fn generate_tools(
    registry: &Registry,
    config: &McpConfig,
) -> Vec<ToolDefinition> {
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
        name: TOOL_LIST_COLLECTIONS.to_string(),
        description: Some("List all collections with their labels and capabilities".to_string()),
        input_schema: json!({ "type": "object", "properties": {} }),
    });

    tools.push(ToolDefinition {
        name: TOOL_DESCRIBE_COLLECTION.to_string(),
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
        name: TOOL_LIST_FIELD_TYPES.to_string(),
        description: Some(
            "List all available field types with descriptions and valid options".to_string(),
        ),
        input_schema: json!({ "type": "object", "properties": {} }),
    });

    tools.push(ToolDefinition {
        name: TOOL_CLI_REFERENCE.to_string(),
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
            name: TOOL_READ_CONFIG_FILE.to_string(),
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
            name: TOOL_WRITE_CONFIG_FILE.to_string(),
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
            name: TOOL_LIST_CONFIG_FILES.to_string(),
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
pub(in crate::mcp) fn parse_tool_name(name: &str, registry: &Registry) -> Option<ParsedTool> {
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
pub(in crate::mcp) fn execute_tool(
    name: &str,
    args: &Value,
    config_dir: &Path,
    ctx: &ToolExecCtx<'_>,
) -> Result<String> {
    // Static tools first
    match name {
        TOOL_LIST_COLLECTIONS => return exec_list_collections(ctx.registry, &ctx.config.mcp),
        TOOL_DESCRIBE_COLLECTION => {
            return exec_describe_collection(args, ctx.registry, &ctx.config.mcp);
        }
        TOOL_LIST_FIELD_TYPES => return exec_list_field_types(),
        TOOL_CLI_REFERENCE => {
            return exec_cli_reference(args.get("command").and_then(Value::as_str));
        }
        TOOL_READ_CONFIG_FILE | TOOL_WRITE_CONFIG_FILE | TOOL_LIST_CONFIG_FILES => {
            if !ctx.config.mcp.config_tools {
                bail!("Config tools are not enabled. Set config_tools = true in [mcp] config.");
            }
            return match name {
                TOOL_READ_CONFIG_FILE => {
                    let path = args
                        .get("path")
                        .and_then(Value::as_str)
                        .context("Missing 'path' argument")?;
                    exec_read_config_file(path, config_dir)
                }
                TOOL_WRITE_CONFIG_FILE => {
                    let path = args
                        .get("path")
                        .and_then(Value::as_str)
                        .context("Missing 'path' argument")?;
                    let content = args
                        .get("content")
                        .and_then(Value::as_str)
                        .context("Missing 'content' argument")?;
                    exec_write_config_file(path, content, config_dir, ctx.client_label)
                }
                TOOL_LIST_CONFIG_FILES => {
                    let subdir = args.get("path").and_then(Value::as_str);
                    exec_list_config_files(subdir, config_dir)
                }
                _ => unreachable!(),
            };
        }
        _ => {}
    }

    // Dynamic CRUD tools
    let Some(parsed) = parse_tool_name(name, ctx.registry) else {
        bail!("Unknown tool: {}", name);
    };

    // Enforce include/exclude at execution time — not just in tools/list.
    // Without this, an attacker who knows a collection slug could directly call
    // e.g. find_<slug> even if the collection was excluded from tool listing.
    if !should_include(&parsed.slug, &ctx.config.mcp) {
        bail!("Tool not available: {}", name);
    }

    let slug = parsed.slug.as_str();
    match parsed.op {
        ToolOp::Find => exec_find(args, slug, ctx),
        ToolOp::FindById => exec_find_by_id(args, slug, ctx),
        ToolOp::Count => exec_count(args, slug, ctx),
        ToolOp::Create => exec_create(args, slug, ctx),
        ToolOp::CreateMany => exec_create_many(args, slug, ctx),
        ToolOp::Update => exec_update(args, slug, ctx),
        ToolOp::UpdateMany => exec_update_many(args, slug, ctx),
        ToolOp::Delete => exec_delete(args, slug, ctx),
        ToolOp::DeleteMany => exec_delete_many(args, slug, ctx),
        ToolOp::Undelete => exec_undelete(args, slug, ctx),
        ToolOp::Unpublish => exec_unpublish(args, slug, ctx),
        ToolOp::ListVersions => exec_list_versions(args, slug, ctx),
        ToolOp::RestoreVersion => exec_restore_version(args, slug, ctx),
        ToolOp::ReadGlobal => exec_read_global(slug, ctx),
        ToolOp::UpdateGlobal => exec_update_global(args, slug, ctx),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;
    use crate::{
        config::{CrapConfig, McpConfig},
        core::{CollectionDefinition, Registry},
        db::{migrate, pool},
        hooks::lifecycle::HookRunner,
        mcp::tools::test_helpers::{make_exec_ctx, make_registry},
    };

    #[test]
    fn generate_tools_basic() {
        let reg = make_registry();
        let config = McpConfig::default();
        let tools = generate_tools(&reg, &config);
        // 2 collections * 5 + 1 global * 2 + 4 introspection = 16
        assert!(tools.len() >= 16);
    }

    #[test]
    fn exclude_collection() {
        let reg = make_registry();
        let config = McpConfig {
            exclude_collections: vec!["users".to_string()],
            ..Default::default()
        };
        let tools = generate_tools(&reg, &config);
        assert!(!tools.iter().any(|t| t.name.contains("users")));
        assert!(tools.iter().any(|t| t.name.contains("posts")));
    }

    #[test]
    fn include_collection() {
        let reg = make_registry();
        let config = McpConfig {
            include_collections: vec!["posts".to_string()],
            ..Default::default()
        };
        let tools = generate_tools(&reg, &config);
        assert!(!tools.iter().any(|t| t.name.contains("users")));
        assert!(tools.iter().any(|t| t.name.contains("posts")));
    }

    #[test]
    fn exclude_takes_precedence() {
        let reg = make_registry();
        let config = McpConfig {
            include_collections: vec!["posts".to_string(), "users".to_string()],
            exclude_collections: vec!["users".to_string()],
            ..Default::default()
        };
        let tools = generate_tools(&reg, &config);
        assert!(!tools.iter().any(|t| t.name.contains("users")));
    }

    #[test]
    fn config_tools_included_when_enabled() {
        let reg = make_registry();
        let config = McpConfig {
            config_tools: true,
            ..Default::default()
        };
        let tools = generate_tools(&reg, &config);
        assert!(tools.iter().any(|t| t.name == "read_config_file"));
        assert!(tools.iter().any(|t| t.name == "write_config_file"));
        assert!(tools.iter().any(|t| t.name == "list_config_files"));
    }

    #[test]
    fn config_tools_excluded_by_default() {
        let reg = make_registry();
        let config = McpConfig::default();
        let tools = generate_tools(&reg, &config);
        assert!(!tools.iter().any(|t| t.name == "read_config_file"));
    }

    #[test]
    fn parse_tool_name_collection() {
        let reg = make_registry();
        let parsed = parse_tool_name("find_posts", &reg).unwrap();
        assert_eq!(parsed.op, ToolOp::Find);
        assert_eq!(parsed.slug, "posts");
    }

    #[test]
    fn parse_tool_name_find_by_id() {
        let reg = make_registry();
        let parsed = parse_tool_name("find_by_id_posts", &reg).unwrap();
        assert_eq!(parsed.op, ToolOp::FindById);
        assert_eq!(parsed.slug, "posts");
    }

    #[test]
    fn parse_tool_name_global() {
        let reg = make_registry();
        let parsed = parse_tool_name("global_read_settings", &reg).unwrap();
        assert_eq!(parsed.op, ToolOp::ReadGlobal);
        assert_eq!(parsed.slug, "settings");
    }

    #[test]
    fn parse_tool_name_unknown() {
        let reg = make_registry();
        assert!(parse_tool_name("find_nonexistent", &reg).is_none());
    }

    #[test]
    fn parse_tool_name_static() {
        let reg = make_registry();
        assert!(parse_tool_name("list_collections", &reg).is_none());
    }

    #[test]
    fn parse_tool_name_create() {
        let reg = make_registry();
        let parsed = parse_tool_name("create_posts", &reg).unwrap();
        assert_eq!(parsed.op, ToolOp::Create);
        assert_eq!(parsed.slug, "posts");
    }

    #[test]
    fn parse_tool_name_update() {
        let reg = make_registry();
        let parsed = parse_tool_name("update_posts", &reg).unwrap();
        assert_eq!(parsed.op, ToolOp::Update);
        assert_eq!(parsed.slug, "posts");
    }

    #[test]
    fn parse_tool_name_delete() {
        let reg = make_registry();
        let parsed = parse_tool_name("delete_posts", &reg).unwrap();
        assert_eq!(parsed.op, ToolOp::Delete);
        assert_eq!(parsed.slug, "posts");
    }

    #[test]
    fn parse_tool_name_global_update() {
        let reg = make_registry();
        let parsed = parse_tool_name("global_update_settings", &reg).unwrap();
        assert_eq!(parsed.op, ToolOp::UpdateGlobal);
        assert_eq!(parsed.slug, "settings");
    }

    #[test]
    fn global_tools_generated() {
        let reg = make_registry();
        let config = McpConfig::default();
        let tools = generate_tools(&reg, &config);
        assert!(tools.iter().any(|t| t.name == "global_read_settings"));
        assert!(tools.iter().any(|t| t.name == "global_update_settings"));
    }

    #[test]
    fn introspection_tools_always_present() {
        let reg = Registry::new();
        let config = McpConfig::default();
        let tools = generate_tools(&reg, &config);
        assert!(tools.iter().any(|t| t.name == "list_collections"));
        assert!(tools.iter().any(|t| t.name == "describe_collection"));
        assert!(tools.iter().any(|t| t.name == "list_field_types"));
        assert!(tools.iter().any(|t| t.name == "cli_reference"));
    }

    #[test]
    fn should_include_basic() {
        let config = McpConfig::default();
        assert!(should_include("posts", &config));
        assert!(should_include("users", &config));
    }

    #[test]
    fn should_include_with_include_list() {
        let config = McpConfig {
            include_collections: vec!["posts".to_string()],
            ..Default::default()
        };
        assert!(should_include("posts", &config));
        assert!(!should_include("users", &config));
    }

    #[test]
    fn should_include_with_exclude_list() {
        let config = McpConfig {
            exclude_collections: vec!["users".to_string()],
            ..Default::default()
        };
        assert!(should_include("posts", &config));
        assert!(!should_include("users", &config));
    }

    #[test]
    fn execute_tool_config_tools_disabled_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        // config_tools is false by default
        assert!(!config.mcp.config_tools);

        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
        let shared = Registry::shared();
        migrate::sync_all(&db_pool, &shared.read().unwrap(), &config.locale).unwrap();
        let registry = Registry::snapshot(&shared);
        let runner = HookRunner::builder()
            .config_dir(tmp.path())
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .unwrap();

        let ctx = make_exec_ctx(&db_pool, &registry, &runner, &config);
        let err = execute_tool(
            "read_config_file",
            &json!({ "path": "init.lua" }),
            tmp.path(),
            &ctx,
        )
        .unwrap_err();
        assert!(err.to_string().contains("config_tools"));
    }

    #[test]
    fn execute_tool_unknown_tool_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();

        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
        let shared = Registry::shared();
        migrate::sync_all(&db_pool, &shared.read().unwrap(), &config.locale).unwrap();
        let registry = Registry::snapshot(&shared);
        let runner = HookRunner::builder()
            .config_dir(tmp.path())
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .unwrap();

        let ctx = make_exec_ctx(&db_pool, &registry, &runner, &config);
        let err = execute_tool("completely_unknown", &json!({}), tmp.path(), &ctx).unwrap_err();
        assert!(err.to_string().contains("Unknown tool"));
    }

    #[test]
    fn execute_tool_excluded_collection_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        config.mcp.exclude_collections = vec!["posts".to_string()];

        let shared = Registry::shared();
        {
            let mut reg = shared.write().unwrap();
            reg.register_collection(CollectionDefinition::new("posts"));
        }

        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
        migrate::sync_all(&db_pool, &shared.read().unwrap(), &config.locale).unwrap();
        let registry = Registry::snapshot(&shared);
        let runner = HookRunner::builder()
            .config_dir(tmp.path())
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .unwrap();

        // An attacker who knows the slug "posts" tries to call find_posts directly
        let ctx = make_exec_ctx(&db_pool, &registry, &runner, &config);
        let err =
            execute_tool("find_posts", &json!({ "limit": 10 }), tmp.path(), &ctx).unwrap_err();
        assert!(
            err.to_string().contains("Tool not available"),
            "Expected 'Tool not available' error, got: {}",
            err
        );
    }

    #[test]
    fn execute_tool_included_collection_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = CrapConfig::test_default();
        config.database.path = "test.db".to_string();
        // Only include "posts", exclude everything else implicitly
        config.mcp.include_collections = vec!["posts".to_string()];

        let shared = Registry::shared();
        {
            let mut reg = shared.write().unwrap();
            reg.register_collection(CollectionDefinition::new("posts"));
            reg.register_collection(CollectionDefinition::new("users"));
        }

        let db_pool = pool::create_pool(tmp.path(), &config).unwrap();
        migrate::sync_all(&db_pool, &shared.read().unwrap(), &config.locale).unwrap();
        let registry = Registry::snapshot(&shared);
        let runner = HookRunner::builder()
            .config_dir(tmp.path())
            .registry(Arc::clone(&registry))
            .config(&config)
            .build()
            .unwrap();

        let ctx = make_exec_ctx(&db_pool, &registry, &runner, &config);

        // find_posts should work (included)
        let result = execute_tool("find_posts", &json!({}), tmp.path(), &ctx);
        assert!(result.is_ok(), "find_posts should succeed: {:?}", result);

        // find_users should be blocked (not in include list)
        let err = execute_tool("find_users", &json!({}), tmp.path(), &ctx).unwrap_err();
        assert!(
            err.to_string().contains("Tool not available"),
            "Expected 'Tool not available' error for users, got: {}",
            err
        );
    }
}
