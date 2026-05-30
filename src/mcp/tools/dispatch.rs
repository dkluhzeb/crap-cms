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
    core::{CollectionDefinition, GlobalDefinition, Registry},
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
    /// Read a global (same as `find_by_id` but for globals)
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

    for (slug, def) in &registry.collections {
        if should_include(slug, config) {
            tools.extend(collection_tools(slug, def));
        }
    }

    for (slug, def) in &registry.globals {
        tools.extend(global_tools(slug, def));
    }

    tools.extend(schema_introspection_tools());

    if config.config_tools {
        tools.extend(config_generation_tools());
    }

    tools
}

/// All MCP tools for a single collection: CRUD + soft-delete/version variants.
fn collection_tools(slug: &str, def: &CollectionDefinition) -> Vec<ToolDefinition> {
    let label = def.display_name();
    let base_desc = def.mcp.description.as_deref().map_or_else(
        || format!("CRUD operations on {label}"),
        std::string::ToString::to_string,
    );
    let schema = |op| collection_input_schema(def, op);

    let mut tools = vec![
        ToolDefinition::new(
            format!("find_{slug}"),
            format!("Query {label} documents. {base_desc}"),
            schema(CrudOp::Find),
        ),
        ToolDefinition::new(
            format!("find_by_id_{slug}"),
            format!("Get a single {label} document by ID"),
            schema(CrudOp::FindById),
        ),
        ToolDefinition::new(
            format!("create_{slug}"),
            format!("Create a new {label} document"),
            schema(CrudOp::Create),
        ),
        ToolDefinition::new(
            format!("create_many_{slug}"),
            format!("Bulk create multiple {label} documents in batched transactions"),
            schema(CrudOp::CreateMany),
        ),
        ToolDefinition::new(
            format!("update_many_{slug}"),
            format!("Bulk update multiple {label} documents matching a filter"),
            schema(CrudOp::UpdateMany),
        ),
        ToolDefinition::new(
            format!("delete_many_{slug}"),
            format!("Bulk delete multiple {label} documents matching a filter"),
            schema(CrudOp::DeleteMany),
        ),
        ToolDefinition::new(
            format!("update_{slug}"),
            format!("Update an existing {label} document"),
            schema(CrudOp::Update),
        ),
        ToolDefinition::new(
            format!("delete_{slug}"),
            format!("Delete a {label} document by ID"),
            schema(CrudOp::Delete),
        ),
        ToolDefinition::new(
            format!("count_{slug}"),
            format!("Count {label} documents matching filters"),
            schema(CrudOp::Count),
        ),
    ];

    if def.has_soft_delete() {
        tools.push(ToolDefinition::new(
            format!("undelete_{slug}"),
            format!("Restore a soft-deleted {label} document"),
            schema(CrudOp::Undelete),
        ));
    }

    if def.versions.is_some() {
        tools.push(ToolDefinition::new(
            format!("unpublish_{slug}"),
            format!("Unpublish a {label} document (set to draft)"),
            schema(CrudOp::Unpublish),
        ));
        tools.push(ToolDefinition::new(
            format!("list_versions_{slug}"),
            format!("List version history for a {label} document"),
            schema(CrudOp::ListVersions),
        ));
        tools.push(ToolDefinition::new(
            format!("restore_version_{slug}"),
            format!("Restore a {label} document to a specific version"),
            schema(CrudOp::RestoreVersion),
        ));
    }

    tools
}

/// MCP tools for a single global: read + update (prefixed `global_` to
/// avoid name collisions with collection tools).
fn global_tools(slug: &str, def: &GlobalDefinition) -> Vec<ToolDefinition> {
    let label = def.display_name();
    vec![
        ToolDefinition::new(
            format!("global_read_{slug}"),
            format!("Read the {label} global document"),
            global_input_schema(def, CrudOp::Find),
        ),
        ToolDefinition::new(
            format!("global_update_{slug}"),
            format!("Update the {label} global document"),
            global_input_schema(def, CrudOp::Update),
        ),
    ]
}

/// Always-on schema introspection tools (list/describe/field types/CLI reference).
fn schema_introspection_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::new(
            TOOL_LIST_COLLECTIONS,
            "List all collections with their labels and capabilities",
            json!({ "type": "object", "properties": {} }),
        ),
        ToolDefinition::new(
            TOOL_DESCRIBE_COLLECTION,
            "Get the full field schema for a collection or global",
            json!({
                "type": "object",
                "properties": {
                    "slug": { "type": "string", "description": "Collection or global slug" }
                },
                "required": ["slug"]
            }),
        ),
        ToolDefinition::new(
            TOOL_LIST_FIELD_TYPES,
            "List all available field types with descriptions and valid options",
            json!({ "type": "object", "properties": {} }),
        ),
        ToolDefinition::new(
            TOOL_CLI_REFERENCE,
            "Get CLI command reference for crap-cms. Returns usage, flags, and examples for all commands or a specific command.",
            json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Specific command to get help for (e.g., 'serve', 'migrate', 'user create'). Omit for full reference."
                    }
                }
            }),
        ),
    ]
}

/// Opt-in config generation tools (`mcp.config_tools = true`).
fn config_generation_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition::new(
            TOOL_READ_CONFIG_FILE,
            "Read a file from the config directory",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path within the config directory" }
                },
                "required": ["path"]
            }),
        ),
        ToolDefinition::new(
            TOOL_WRITE_CONFIG_FILE,
            "Write a file to the config directory (creates parent dirs)",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path within the config directory" },
                    "content": { "type": "string", "description": "File content to write" }
                },
                "required": ["path", "content"]
            }),
        ),
        ToolDefinition::new(
            TOOL_LIST_CONFIG_FILES,
            "List files in the config directory",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Subdirectory to list (default: root)" }
                }
            }),
        ),
    ]
}

/// Parse a tool name like "`find_posts`" into (op, slug).
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
        bail!("Unknown tool: {name}");
    };

    // Enforce include/exclude at execution time — not just in tools/list.
    // Without this, an attacker who knows a collection slug could directly call
    // e.g. find_<slug> even if the collection was excluded from tool listing.
    if !should_include(&parsed.slug, &ctx.config.mcp) {
        bail!("Tool not available: {name}");
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
        ToolOp::ReadGlobal => exec_read_global(args, slug, ctx),
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
            "Expected 'Tool not available' error, got: {err}"
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
        assert!(result.is_ok(), "find_posts should succeed: {result:?}");

        // find_users should be blocked (not in include list)
        let err = execute_tool("find_users", &json!({}), tmp.path(), &ctx).unwrap_err();
        assert!(
            err.to_string().contains("Tool not available"),
            "Expected 'Tool not available' error for users, got: {err}"
        );
    }
}
