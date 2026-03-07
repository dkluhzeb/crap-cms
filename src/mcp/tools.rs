//! MCP tool generation from Registry and tool execution.
//!
//! **Security model:** MCP operates with full access — no collection-level or field-level
//! access control is applied. This is intentional: MCP is a programmatic API surface
//! (like Lua's `overrideAccess = true`) gated by transport-level auth (API key for HTTP,
//! process-level access for stdio). Access control Lua functions are designed for per-user
//! restrictions and don't apply to machine-to-machine access.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use serde_json::{json, Value};

use crate::config::McpConfig;
use crate::core::Registry;
#[cfg(test)]
use crate::core::{CollectionDefinition, collection::GlobalDefinition};
use crate::core::document::Document;
use crate::db::DbPool;
use crate::db::query::{self, FindQuery};
use crate::hooks::lifecycle::HookRunner;

use super::protocol::ToolDefinition;
use super::schema::{CrudOp, collection_input_schema, global_input_schema};

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
        let base_desc = def.mcp.description.as_deref()
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
        description: Some("List all available field types with descriptions and valid options".to_string()),
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
pub fn parse_tool_name<'a>(name: &'a str, registry: &Registry) -> Option<ParsedTool> {
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
                return Some(ParsedTool { op, slug: slug.to_string() });
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
                return Some(ParsedTool { op, slug: slug.to_string() });
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
            ToolOp::Create => exec_create(args, &parsed.slug, registry, pool, runner),
            ToolOp::Update => exec_update(args, &parsed.slug, registry, pool, runner),
            ToolOp::Delete => exec_delete(args, &parsed.slug, registry, pool, runner),
            ToolOp::ReadGlobal => exec_read_global(&parsed.slug, registry, pool),
            ToolOp::UpdateGlobal => exec_update_global(args, &parsed.slug, registry, pool, runner),
        };
    }

    bail!("Unknown tool: {}", name)
}

fn exec_list_collections(registry: &Registry, mcp_config: &McpConfig) -> Result<String> {
    let mut result = Vec::new();
    for (slug, def) in &registry.collections {
        if !should_include(slug, mcp_config) {
            continue;
        }
        result.push(json!({
            "slug": slug,
            "label": def.display_name(),
            "fields": def.fields.len(),
            "has_auth": def.is_auth_collection(),
            "has_upload": def.is_upload_collection(),
            "has_drafts": def.has_drafts(),
        }));
    }
    for (slug, def) in &registry.globals {
        result.push(json!({
            "slug": slug,
            "label": def.display_name(),
            "type": "global",
            "fields": def.fields.len(),
        }));
    }
    Ok(serde_json::to_string_pretty(&result)?)
}

fn exec_describe_collection(args: &Value, registry: &Registry, mcp_config: &McpConfig) -> Result<String> {
    let slug = args.get("slug").and_then(|v| v.as_str())
        .context("Missing 'slug' argument")?;

    if let Some(def) = registry.collections.get(slug) {
        if !should_include(slug, mcp_config) {
            bail!("Unknown collection or global: {}", slug);
        }
        let schema = collection_input_schema(def, CrudOp::Create);
        let result = json!({
            "slug": slug,
            "type": "collection",
            "label": def.display_name(),
            "timestamps": def.timestamps,
            "has_auth": def.is_auth_collection(),
            "has_upload": def.is_upload_collection(),
            "has_drafts": def.has_drafts(),
            "schema": schema,
        });
        return Ok(serde_json::to_string_pretty(&result)?);
    }

    if let Some(def) = registry.globals.get(slug) {
        let schema = global_input_schema(def, CrudOp::Update);
        let result = json!({
            "slug": slug,
            "type": "global",
            "label": def.display_name(),
            "schema": schema,
        });
        return Ok(serde_json::to_string_pretty(&result)?);
    }

    bail!("Unknown collection or global: {}", slug)
}

fn exec_list_field_types() -> Result<String> {
    let types = json!([
        { "name": "text", "description": "Single-line text input", "json_schema_type": "string", "supports_has_many": false, "supports_sub_fields": false, "supports_options": false },
        { "name": "number", "description": "Numeric input (integer or float)", "json_schema_type": "number", "supports_has_many": false, "supports_sub_fields": false, "supports_options": false },
        { "name": "textarea", "description": "Multi-line text input", "json_schema_type": "string", "supports_has_many": false, "supports_sub_fields": false, "supports_options": false },
        { "name": "select", "description": "Dropdown select from predefined options", "json_schema_type": "string", "supports_has_many": true, "supports_sub_fields": false, "supports_options": true },
        { "name": "radio", "description": "Radio button group from predefined options", "json_schema_type": "string", "supports_has_many": false, "supports_sub_fields": false, "supports_options": true },
        { "name": "checkbox", "description": "Boolean checkbox (true/false)", "json_schema_type": "boolean", "supports_has_many": false, "supports_sub_fields": false, "supports_options": false },
        { "name": "date", "description": "Date/datetime picker", "json_schema_type": "string", "supports_has_many": false, "supports_sub_fields": false, "supports_options": false },
        { "name": "email", "description": "Email address input with validation", "json_schema_type": "string", "supports_has_many": false, "supports_sub_fields": false, "supports_options": false },
        { "name": "json", "description": "Raw JSON data stored as text", "json_schema_type": "string", "supports_has_many": false, "supports_sub_fields": false, "supports_options": false },
        { "name": "richtext", "description": "Rich text editor (HTML content)", "json_schema_type": "string", "supports_has_many": false, "supports_sub_fields": false, "supports_options": false },
        { "name": "code", "description": "Code editor with syntax highlighting", "json_schema_type": "string", "supports_has_many": false, "supports_sub_fields": false, "supports_options": false },
        { "name": "relationship", "description": "Reference to document(s) in another collection", "json_schema_type": "string", "supports_has_many": true, "supports_sub_fields": false, "supports_options": false },
        { "name": "array", "description": "Repeatable group of sub-fields (stored in join table)", "json_schema_type": "array", "supports_has_many": false, "supports_sub_fields": true, "supports_options": false },
        { "name": "group", "description": "Named group of sub-fields (columns prefixed with group name)", "json_schema_type": "object", "supports_has_many": false, "supports_sub_fields": true, "supports_options": false },
        { "name": "upload", "description": "File upload field referencing an upload collection", "json_schema_type": "string", "supports_has_many": true, "supports_sub_fields": false, "supports_options": false },
        { "name": "blocks", "description": "Flexible content blocks with different block types", "json_schema_type": "array", "supports_has_many": false, "supports_sub_fields": true, "supports_options": false },
        { "name": "row", "description": "Layout-only horizontal container. Sub-fields promoted to parent level (no prefix)", "json_schema_type": "null", "supports_has_many": false, "supports_sub_fields": true, "supports_options": false },
        { "name": "collapsible", "description": "Layout-only collapsible container. Sub-fields promoted to parent level (no prefix)", "json_schema_type": "null", "supports_has_many": false, "supports_sub_fields": true, "supports_options": false },
        { "name": "tabs", "description": "Layout-only tabbed container. Sub-fields promoted to parent level (no prefix)", "json_schema_type": "null", "supports_has_many": false, "supports_sub_fields": true, "supports_options": false },
        { "name": "join", "description": "Virtual reverse-relationship field. Shows documents from another collection that reference this document. No stored data.", "json_schema_type": "null", "supports_has_many": false, "supports_sub_fields": false, "supports_options": false },
    ]);
    Ok(serde_json::to_string_pretty(&types)?)
}

fn exec_cli_reference(args: &Value) -> Result<String> {
    let command = args.get("command").and_then(|v| v.as_str());

    match command {
        None => {
            let overview = json!({
                "binary": "crap-cms",
                "description": "Crap CMS - Headless CMS with Lua hooks",
                "usage": "crap-cms <COMMAND> [OPTIONS]",
                "commands": [
                    { "name": "serve", "description": "Start the admin UI and gRPC servers" },
                    { "name": "status", "description": "Show project status (collections, globals, migrations)" },
                    { "name": "init", "description": "Scaffold a new config directory" },
                    { "name": "make", "description": "Generate scaffolding files (collection, global, hook, job)" },
                    { "name": "blueprint", "description": "Manage saved blueprints (save, use, list, remove)" },
                    { "name": "user", "description": "User management for auth collections (create, list, delete, lock, unlock, change-password)" },
                    { "name": "migrate", "description": "Run database migrations (create, up, down, list, fresh)" },
                    { "name": "backup", "description": "Backup database and optionally uploads" },
                    { "name": "db", "description": "Database tools (console, cleanup)" },
                    { "name": "export", "description": "Export collection data to JSON" },
                    { "name": "import", "description": "Import collection data from JSON" },
                    { "name": "typegen", "description": "Generate typed definitions from collection schemas" },
                    { "name": "proto", "description": "Export the embedded content.proto file" },
                    { "name": "templates", "description": "List and extract default admin templates and static files" },
                    { "name": "jobs", "description": "Manage background jobs (list, trigger, status, purge, healthcheck)" },
                    { "name": "images", "description": "Manage image processing queue (list, stats, retry, purge)" },
                    { "name": "mcp", "description": "Start the MCP (Model Context Protocol) server (stdio transport)" },
                ]
            });
            Ok(serde_json::to_string_pretty(&overview)?)
        }
        Some(cmd) => {
            let detail = match cmd {
                "serve" => json!({
                    "command": "crap-cms serve <CONFIG_DIR>",
                    "description": "Start the admin UI and gRPC servers",
                    "flags": [
                        { "flag": "-d, --detach", "description": "Run in the background (detached)" }
                    ],
                    "examples": [
                        "crap-cms serve ./my-site",
                        "crap-cms serve ./my-site --detach"
                    ]
                }),
                "status" => json!({
                    "command": "crap-cms status <CONFIG_DIR>",
                    "description": "Show project status (collections, globals, migrations)",
                    "examples": ["crap-cms status ./my-site"]
                }),
                "init" => json!({
                    "command": "crap-cms init [DIR]",
                    "description": "Scaffold a new config directory with default structure",
                    "args": [
                        { "arg": "DIR", "description": "Directory to create (default: ./crap-cms)" }
                    ],
                    "examples": [
                        "crap-cms init",
                        "crap-cms init ./my-site"
                    ]
                }),
                "make" | "make collection" | "make global" | "make hook" | "make job" => json!({
                    "command": "crap-cms make <SUBCOMMAND>",
                    "description": "Generate scaffolding files",
                    "subcommands": [
                        {
                            "name": "collection",
                            "usage": "crap-cms make collection <CONFIG_DIR> [SLUG] [OPTIONS]",
                            "description": "Generate a collection Lua file",
                            "flags": [
                                { "flag": "-F, --fields <FIELDS>", "description": "Inline field shorthand (e.g., 'title:text:required,status:select')" },
                                { "flag": "-T, --no-timestamps", "description": "Disable timestamps" },
                                { "flag": "--auth", "description": "Enable auth (email/password login)" },
                                { "flag": "--upload", "description": "Enable uploads (file upload collection)" },
                                { "flag": "--versions", "description": "Enable versioning (draft/publish)" },
                                { "flag": "--no-input", "description": "Non-interactive mode" },
                                { "flag": "-f, --force", "description": "Overwrite existing file" }
                            ],
                            "examples": [
                                "crap-cms make collection ./my-site posts -F 'title:text:required,body:richtext,status:select'",
                                "crap-cms make collection ./my-site users --auth --no-input"
                            ]
                        },
                        {
                            "name": "global",
                            "usage": "crap-cms make global <CONFIG_DIR> [SLUG] [OPTIONS]",
                            "description": "Generate a global Lua file",
                            "flags": [
                                { "flag": "-F, --fields <FIELDS>", "description": "Inline field shorthand" },
                                { "flag": "-f, --force", "description": "Overwrite existing file" }
                            ]
                        },
                        {
                            "name": "hook",
                            "usage": "crap-cms make hook <CONFIG_DIR> [NAME] [OPTIONS]",
                            "description": "Generate a hook file",
                            "flags": [
                                { "flag": "-t, --type <TYPE>", "description": "Hook type: collection, field, or access" },
                                { "flag": "-c, --collection <SLUG>", "description": "Target collection slug" },
                                { "flag": "-l, --position <POS>", "description": "Lifecycle position (e.g., before_change, after_read)" },
                                { "flag": "-F, --field <NAME>", "description": "Target field name (field hooks only)" },
                                { "flag": "--force", "description": "Overwrite existing file" }
                            ]
                        },
                        {
                            "name": "job",
                            "usage": "crap-cms make job <CONFIG_DIR> [SLUG] [OPTIONS]",
                            "description": "Generate a job Lua file",
                            "flags": [
                                { "flag": "-s, --schedule <CRON>", "description": "Cron schedule expression" },
                                { "flag": "-q, --queue <NAME>", "description": "Queue name (default: 'default')" },
                                { "flag": "-r, --retries <N>", "description": "Max retry attempts (default: 0)" },
                                { "flag": "-t, --timeout <SECS>", "description": "Timeout in seconds (default: 60)" },
                                { "flag": "-f, --force", "description": "Overwrite existing file" }
                            ]
                        }
                    ]
                }),
                "blueprint" | "blueprint save" | "blueprint use" | "blueprint list" | "blueprint remove" => json!({
                    "command": "crap-cms blueprint <SUBCOMMAND>",
                    "description": "Manage saved blueprints",
                    "subcommands": [
                        { "name": "save", "usage": "crap-cms blueprint save <CONFIG_DIR> <NAME> [-f]", "description": "Save a config directory as a reusable blueprint" },
                        { "name": "use", "usage": "crap-cms blueprint use [NAME] [DIR]", "description": "Create a new project from a saved blueprint" },
                        { "name": "list", "usage": "crap-cms blueprint list", "description": "List all saved blueprints" },
                        { "name": "remove", "usage": "crap-cms blueprint remove [NAME]", "description": "Remove a saved blueprint" }
                    ]
                }),
                "user" | "user create" | "user list" | "user delete" | "user lock" | "user unlock" | "user change-password" => json!({
                    "command": "crap-cms user <SUBCOMMAND>",
                    "description": "User management for auth collections",
                    "subcommands": [
                        {
                            "name": "create",
                            "usage": "crap-cms user create <CONFIG_DIR> [OPTIONS]",
                            "description": "Create a new user",
                            "flags": [
                                { "flag": "-c, --collection <SLUG>", "description": "Auth collection slug (default: users)" },
                                { "flag": "-e, --email <EMAIL>", "description": "User email" },
                                { "flag": "-p, --password <PW>", "description": "User password (omit for interactive prompt)" },
                                { "flag": "-f, --field <KEY=VALUE>", "description": "Extra fields (repeatable)" }
                            ],
                            "examples": [
                                "crap-cms user create ./my-site -e admin@example.com",
                                "crap-cms user create ./my-site -e admin@example.com -p secret -f role=admin -f name='Admin'"
                            ]
                        },
                        { "name": "list", "usage": "crap-cms user list <CONFIG_DIR> [-c <SLUG>]", "description": "List users in an auth collection" },
                        {
                            "name": "delete",
                            "usage": "crap-cms user delete <CONFIG_DIR> [OPTIONS]",
                            "description": "Delete a user",
                            "flags": [
                                { "flag": "-e, --email <EMAIL>", "description": "User email" },
                                { "flag": "--id <ID>", "description": "User ID" },
                                { "flag": "-y, --confirm", "description": "Skip confirmation prompt" }
                            ]
                        },
                        { "name": "lock", "usage": "crap-cms user lock <CONFIG_DIR> [-e <EMAIL>] [--id <ID>]", "description": "Lock a user account (prevent login)" },
                        { "name": "unlock", "usage": "crap-cms user unlock <CONFIG_DIR> [-e <EMAIL>] [--id <ID>]", "description": "Unlock a user account" },
                        {
                            "name": "change-password",
                            "usage": "crap-cms user change-password <CONFIG_DIR> [OPTIONS]",
                            "description": "Change a user's password",
                            "flags": [
                                { "flag": "-e, --email <EMAIL>", "description": "User email" },
                                { "flag": "--id <ID>", "description": "User ID" },
                                { "flag": "-p, --password <PW>", "description": "New password (omit for interactive)" }
                            ]
                        }
                    ]
                }),
                "migrate" | "migrate create" | "migrate up" | "migrate down" | "migrate list" | "migrate fresh" => json!({
                    "command": "crap-cms migrate <CONFIG_DIR> <SUBCOMMAND>",
                    "description": "Run database migrations",
                    "subcommands": [
                        { "name": "create", "usage": "crap-cms migrate <CONFIG_DIR> create <NAME>", "description": "Create a new migration file" },
                        { "name": "up", "usage": "crap-cms migrate <CONFIG_DIR> up", "description": "Schema sync + run pending Lua data migrations" },
                        { "name": "down", "usage": "crap-cms migrate <CONFIG_DIR> down [-s <N>]", "description": "Rollback last N data migrations (default: 1)" },
                        { "name": "list", "usage": "crap-cms migrate <CONFIG_DIR> list", "description": "Show all migration files with applied/pending status" },
                        { "name": "fresh", "usage": "crap-cms migrate <CONFIG_DIR> fresh -y", "description": "Drop all tables, recreate from Lua definitions, run all migrations (destructive!)" }
                    ],
                    "examples": [
                        "crap-cms migrate ./my-site up",
                        "crap-cms migrate ./my-site create add_categories",
                        "crap-cms migrate ./my-site down -s 2",
                        "crap-cms migrate ./my-site fresh -y"
                    ]
                }),
                "backup" => json!({
                    "command": "crap-cms backup <CONFIG_DIR> [OPTIONS]",
                    "description": "Backup database and optionally uploads",
                    "flags": [
                        { "flag": "-o, --output <DIR>", "description": "Output directory (default: <config_dir>/backups/)" },
                        { "flag": "-i, --include-uploads", "description": "Also compress the uploads directory" }
                    ],
                    "examples": [
                        "crap-cms backup ./my-site",
                        "crap-cms backup ./my-site -o /backups -i"
                    ]
                }),
                "db" | "db console" | "db cleanup" => json!({
                    "command": "crap-cms db <SUBCOMMAND>",
                    "description": "Database tools",
                    "subcommands": [
                        { "name": "console", "usage": "crap-cms db console <CONFIG_DIR>", "description": "Open an interactive SQLite console" },
                        {
                            "name": "cleanup",
                            "usage": "crap-cms db cleanup <CONFIG_DIR> [--confirm]",
                            "description": "Detect and optionally remove orphan columns not in Lua definitions",
                            "flags": [
                                { "flag": "--confirm", "description": "Actually drop orphan columns (default: dry-run report)" }
                            ]
                        }
                    ]
                }),
                "export" => json!({
                    "command": "crap-cms export <CONFIG_DIR> [OPTIONS]",
                    "description": "Export collection data to JSON",
                    "flags": [
                        { "flag": "-c, --collection <SLUG>", "description": "Export only this collection (default: all)" },
                        { "flag": "-o, --output <FILE>", "description": "Output file (default: stdout)" }
                    ],
                    "examples": [
                        "crap-cms export ./my-site",
                        "crap-cms export ./my-site -c posts -o posts.json"
                    ]
                }),
                "import" => json!({
                    "command": "crap-cms import <CONFIG_DIR> <FILE> [OPTIONS]",
                    "description": "Import collection data from JSON",
                    "flags": [
                        { "flag": "-c, --collection <SLUG>", "description": "Import only this collection (default: all in file)" }
                    ],
                    "examples": [
                        "crap-cms import ./my-site backup.json",
                        "crap-cms import ./my-site posts.json -c posts"
                    ]
                }),
                "typegen" => json!({
                    "command": "crap-cms typegen <CONFIG_DIR> [OPTIONS]",
                    "description": "Generate typed definitions from collection schemas",
                    "flags": [
                        { "flag": "-l, --lang <LANG>", "description": "Output language: lua, ts, go, py, rs, all (default: lua)" },
                        { "flag": "-o, --output <DIR>", "description": "Output directory (default: <config>/types/)" }
                    ],
                    "examples": [
                        "crap-cms typegen ./my-site -l ts",
                        "crap-cms typegen ./my-site -l all -o ./types"
                    ]
                }),
                "proto" => json!({
                    "command": "crap-cms proto [OPTIONS]",
                    "description": "Export the embedded content.proto file for gRPC client codegen",
                    "flags": [
                        { "flag": "-o, --output <PATH>", "description": "Output path (file or directory). Omit to write to stdout." }
                    ],
                    "examples": [
                        "crap-cms proto",
                        "crap-cms proto -o ./proto/content.proto"
                    ]
                }),
                "templates" | "templates list" | "templates extract" => json!({
                    "command": "crap-cms templates <SUBCOMMAND>",
                    "description": "List and extract default admin templates and static files",
                    "subcommands": [
                        {
                            "name": "list",
                            "usage": "crap-cms templates list [OPTIONS]",
                            "description": "List all available default templates and static files",
                            "flags": [
                                { "flag": "-t, --type <TYPE>", "description": "Filter: 'templates' or 'static' (default: both)" },
                                { "flag": "-v, --verbose", "description": "Show full file tree with sizes" }
                            ]
                        },
                        {
                            "name": "extract",
                            "usage": "crap-cms templates extract <CONFIG_DIR> [PATHS...] [OPTIONS]",
                            "description": "Extract default files into config directory for customization",
                            "flags": [
                                { "flag": "-a, --all", "description": "Extract all files" },
                                { "flag": "-t, --type <TYPE>", "description": "Filter: 'templates' or 'static' (only with --all)" },
                                { "flag": "-f, --force", "description": "Overwrite existing files" }
                            ]
                        }
                    ]
                }),
                "jobs" | "jobs list" | "jobs trigger" | "jobs status" | "jobs purge" | "jobs healthcheck" => json!({
                    "command": "crap-cms jobs <SUBCOMMAND>",
                    "description": "Manage background jobs",
                    "subcommands": [
                        { "name": "list", "usage": "crap-cms jobs list <CONFIG_DIR>", "description": "List defined jobs and recent runs" },
                        {
                            "name": "trigger",
                            "usage": "crap-cms jobs trigger <CONFIG_DIR> <SLUG> [OPTIONS]",
                            "description": "Trigger a job manually",
                            "flags": [
                                { "flag": "-d, --data <JSON>", "description": "JSON data to pass to the job" }
                            ]
                        },
                        {
                            "name": "status",
                            "usage": "crap-cms jobs status <CONFIG_DIR> [OPTIONS]",
                            "description": "Show job run history",
                            "flags": [
                                { "flag": "--id <ID>", "description": "Show a single job run by ID" },
                                { "flag": "-s, --slug <SLUG>", "description": "Filter by job slug" },
                                { "flag": "-l, --limit <N>", "description": "Max results (default: 20)" }
                            ]
                        },
                        {
                            "name": "purge",
                            "usage": "crap-cms jobs purge <CONFIG_DIR> [OPTIONS]",
                            "description": "Clean up old completed/failed job runs",
                            "flags": [
                                { "flag": "--older-than <DURATION>", "description": "Delete runs older than this (e.g., '7d', '24h'). Default: 7d" }
                            ]
                        },
                        { "name": "healthcheck", "usage": "crap-cms jobs healthcheck <CONFIG_DIR>", "description": "Check job system health" }
                    ]
                }),
                "images" | "images list" | "images stats" | "images retry" | "images purge" => json!({
                    "command": "crap-cms images <SUBCOMMAND>",
                    "description": "Manage image processing queue",
                    "subcommands": [
                        {
                            "name": "list",
                            "usage": "crap-cms images list <CONFIG_DIR> [OPTIONS]",
                            "description": "List image processing queue entries",
                            "flags": [
                                { "flag": "-s, --status <STATUS>", "description": "Filter: pending, processing, completed, failed" },
                                { "flag": "-l, --limit <N>", "description": "Max entries (default: 20)" }
                            ]
                        },
                        { "name": "stats", "usage": "crap-cms images stats <CONFIG_DIR>", "description": "Show queue statistics by status" },
                        {
                            "name": "retry",
                            "usage": "crap-cms images retry <CONFIG_DIR> [OPTIONS]",
                            "description": "Retry failed queue entries",
                            "flags": [
                                { "flag": "--id <ID>", "description": "Retry a specific entry by ID" },
                                { "flag": "--all", "description": "Retry all failed entries" },
                                { "flag": "-y, --confirm", "description": "Confirm retry all (required with --all)" }
                            ]
                        },
                        {
                            "name": "purge",
                            "usage": "crap-cms images purge <CONFIG_DIR> [OPTIONS]",
                            "description": "Purge old completed/failed entries",
                            "flags": [
                                { "flag": "--older-than <DURATION>", "description": "Delete entries older than this (e.g., '7d'). Default: 7d" }
                            ]
                        }
                    ]
                }),
                "mcp" => json!({
                    "command": "crap-cms mcp <CONFIG_DIR>",
                    "description": "Start the MCP (Model Context Protocol) server using stdio transport",
                    "examples": ["crap-cms mcp ./my-site"]
                }),
                _ => json!({ "error": format!("Unknown command: '{}'. Call cli_reference without a command argument to see all available commands.", cmd) }),
            };
            Ok(serde_json::to_string_pretty(&detail)?)
        }
    }
}

/// Safely resolve a relative path within the config directory.
/// Rejects absolute paths, `..` components, and symlinks escaping the boundary.
fn safe_config_path(config_dir: &Path, relative: &str) -> Result<std::path::PathBuf> {
    // Reject absolute paths outright (on Unix, Path::join with absolute replaces the base)
    if std::path::Path::new(relative).is_absolute() {
        bail!("Absolute paths not allowed");
    }
    // Reject .. traversal
    if relative.contains("..") {
        bail!("Path traversal not allowed");
    }
    let full_path = config_dir.join(relative);
    // Canonicalize and verify the result stays within config_dir.
    // For read/list, the file/dir must already exist for canonicalize to work.
    // For write, the parent must exist (create_dir_all handles this upstream).
    let canonical_base = config_dir.canonicalize()
        .with_context(|| format!("Config dir not found: {}", config_dir.display()))?;
    // If file exists, canonicalize it. Otherwise verify the parent is inside config_dir.
    if full_path.exists() {
        let canonical = full_path.canonicalize()?;
        if !canonical.starts_with(&canonical_base) {
            bail!("Path escapes config directory");
        }
    } else if let Some(parent) = full_path.parent() {
        // For new files, check that the parent stays inside config_dir
        if parent.exists() {
            let canonical_parent = parent.canonicalize()?;
            if !canonical_parent.starts_with(&canonical_base) {
                bail!("Path escapes config directory");
            }
        }
    }
    Ok(full_path)
}

fn exec_read_config_file(args: &Value, config_dir: &Path) -> Result<String> {
    let path = args.get("path").and_then(|v| v.as_str())
        .context("Missing 'path' argument")?;
    let full_path = safe_config_path(config_dir, path)?;
    let content = std::fs::read_to_string(&full_path)
        .with_context(|| format!("Failed to read {}", full_path.display()))?;
    Ok(content)
}

fn exec_write_config_file(args: &Value, config_dir: &Path) -> Result<String> {
    let path = args.get("path").and_then(|v| v.as_str())
        .context("Missing 'path' argument")?;
    let content = args.get("content").and_then(|v| v.as_str())
        .context("Missing 'content' argument")?;
    let full_path = safe_config_path(config_dir, path)?;
    if let Some(parent) = full_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tracing::info!("MCP write_config_file: {}", path);
    std::fs::write(&full_path, content)
        .with_context(|| format!("Failed to write {}", full_path.display()))?;
    Ok(json!({ "written": path }).to_string())
}

fn exec_list_config_files(args: &Value, config_dir: &Path) -> Result<String> {
    let subdir = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let dir = if subdir.is_empty() {
        config_dir.to_path_buf()
    } else {
        safe_config_path(config_dir, subdir)?
    };
    let mut files = Vec::new();
    if dir.is_dir() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type()?.is_dir();
            files.push(json!({
                "name": name,
                "type": if is_dir { "directory" } else { "file" },
            }));
        }
    }
    Ok(serde_json::to_string_pretty(&files)?)
}

/// Parse JSON `where` object into filter clauses.
/// Supports `{ field: "value" }` (equals) and `{ field: { op: value } }` (operator-based).
fn parse_where_filters(args: &Value) -> Vec<query::FilterClause> {
    let where_val = match args.get("where") {
        Some(v) if v.is_object() => v,
        _ => return Vec::new(),
    };
    let map = match where_val.as_object() {
        Some(m) => m,
        None => return Vec::new(),
    };

    let mut clauses = Vec::new();
    for (field, value) in map {
        match value {
            Value::String(s) => {
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.clone(),
                    op: query::FilterOp::Equals(s.clone()),
                }));
            }
            Value::Number(n) => {
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.clone(),
                    op: query::FilterOp::Equals(n.to_string()),
                }));
            }
            Value::Bool(b) => {
                clauses.push(query::FilterClause::Single(query::Filter {
                    field: field.clone(),
                    op: query::FilterOp::Equals(if *b { "1" } else { "0" }.to_string()),
                }));
            }
            Value::Object(ops) => {
                for (op_name, op_value) in ops {
                    // Handle array-valued operators (in, not_in)
                    if matches!(op_name.as_str(), "in" | "not_in") {
                        if let Some(arr) = op_value.as_array() {
                            let vals: Vec<String> = arr.iter().filter_map(|v| match v {
                                Value::String(s) => Some(s.clone()),
                                Value::Number(n) => Some(n.to_string()),
                                _ => None,
                            }).collect();
                            let op = match op_name.as_str() {
                                "in" => query::FilterOp::In(vals),
                                "not_in" => query::FilterOp::NotIn(vals),
                                _ => unreachable!(),
                            };
                            clauses.push(query::FilterClause::Single(query::Filter {
                                field: field.clone(),
                                op,
                            }));
                        }
                        continue;
                    }
                    // Handle value-less operators (exists, not_exists)
                    if matches!(op_name.as_str(), "exists" | "not_exists") {
                        let op = match op_name.as_str() {
                            "exists" => query::FilterOp::Exists,
                            "not_exists" => query::FilterOp::NotExists,
                            _ => unreachable!(),
                        };
                        clauses.push(query::FilterClause::Single(query::Filter {
                            field: field.clone(),
                            op,
                        }));
                        continue;
                    }
                    // Scalar-valued operators
                    let val_str = match op_value {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => (if *b { "1" } else { "0" }).to_string(),
                        _ => continue,
                    };
                    let op = match op_name.as_str() {
                        "equals" => query::FilterOp::Equals(val_str),
                        "not_equals" => query::FilterOp::NotEquals(val_str),
                        "contains" => query::FilterOp::Contains(val_str),
                        "greater_than" => query::FilterOp::GreaterThan(val_str),
                        "greater_than_equal" => query::FilterOp::GreaterThanOrEqual(val_str),
                        "less_than" => query::FilterOp::LessThan(val_str),
                        "less_than_equal" => query::FilterOp::LessThanOrEqual(val_str),
                        "like" => query::FilterOp::Like(val_str),
                        _ => continue,
                    };
                    clauses.push(query::FilterClause::Single(query::Filter {
                        field: field.clone(),
                        op,
                    }));
                }
            }
            _ => {}
        }
    }
    clauses
}

fn doc_to_json(doc: &Document) -> Value {
    let mut obj = serde_json::Map::new();
    obj.insert("id".to_string(), Value::String(doc.id.clone()));
    for (k, v) in &doc.fields {
        obj.insert(k.clone(), v.clone());
    }
    if let Some(ref ca) = doc.created_at {
        obj.insert("created_at".to_string(), Value::String(ca.clone()));
    }
    if let Some(ref ua) = doc.updated_at {
        obj.insert("updated_at".to_string(), Value::String(ua.clone()));
    }
    Value::Object(obj)
}

fn exec_find(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    _runner: &HookRunner,
    config: &crate::config::CrapConfig,
) -> Result<String> {
    let def = registry.collections.get(slug)
        .context("Collection not found")?;
    let conn = pool.get().context("DB connection")?;

    let limit = args.get("limit").and_then(|v| v.as_i64());
    let limit = query::apply_pagination_limits(limit, config.pagination.default_limit, config.pagination.max_limit);
    let offset = args.get("offset").and_then(|v| v.as_i64());
    let order_by = args.get("order_by").and_then(|v| v.as_str()).map(|s| s.to_string());
    let search = args.get("search").and_then(|v| v.as_str()).map(|s| s.to_string());
    let depth = args.get("depth").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    let depth = depth.min(config.depth.max_depth as i32);
    let filters = parse_where_filters(args);

    let fq = FindQuery {
        filters,
        order_by,
        limit: Some(limit),
        offset,
        select: None,
        after_cursor: None,
        before_cursor: None,
        search,
    };
    let mut docs = query::find(&conn, slug, def, &fq, None)?;
    let total = query::count(&conn, slug, def, &fq.filters, None)?;

    if depth > 0 {
        query::populate_relationships_batch(&conn, registry, slug, def, &mut docs, depth, None, None)?;
    }

    let result = json!({
        "docs": docs.iter().map(doc_to_json).collect::<Vec<_>>(),
        "totalDocs": total,
        "limit": limit,
        "offset": offset.unwrap_or(0),
    });
    Ok(serde_json::to_string_pretty(&result)?)
}

fn exec_find_by_id(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    config: &crate::config::CrapConfig,
) -> Result<String> {
    let id = args.get("id").and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;

    let def = registry.collections.get(slug)
        .context("Collection not found")?;
    let conn = pool.get().context("DB connection")?;

    let depth = args.get("depth").and_then(|v| v.as_i64())
        .unwrap_or(config.depth.default_depth as i64) as i32;
    let depth = depth.min(config.depth.max_depth as i32);

    let mut doc = match query::find_by_id(&conn, slug, def, id, None)? {
        Some(d) => d,
        None => return Ok(json!({ "error": "Document not found" }).to_string()),
    };

    if depth > 0 {
        let mut visited = std::collections::HashSet::new();
        query::populate_relationships(&conn, registry, slug, def, &mut doc, depth, &mut visited, None, None)?;
    }

    Ok(serde_json::to_string_pretty(&doc_to_json(&doc))?)
}

fn exec_create(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
) -> Result<String> {
    let def = registry.collections.get(slug)
        .context("Collection not found")?;

    // Extract password for auth collections
    let password = if def.is_auth_collection() {
        args.get("password").and_then(|v| v.as_str()).map(|s| s.to_string())
    } else {
        None
    };

    // Convert args to string map for create
    let mut data = HashMap::new();
    let mut join_data = HashMap::new();
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            if k == "password" { continue; }
            match v {
                Value::String(s) => { data.insert(k.clone(), s.clone()); }
                Value::Number(n) => { data.insert(k.clone(), n.to_string()); }
                Value::Bool(b) => { data.insert(k.clone(), if *b { "1".to_string() } else { "0".to_string() }); }
                Value::Array(_) | Value::Object(_) => { join_data.insert(k.clone(), v.clone()); }
                Value::Null => {}
            }
        }
    }

    let (doc, _ctx) = crate::service::create_document(
        pool, runner, slug, def, data, &join_data,
        password.as_deref(), None, None, None, false,
    )?;

    tracing::info!("MCP create {}: {}", slug, doc.id);
    Ok(serde_json::to_string_pretty(&doc_to_json(&doc))?)
}

fn exec_update(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
) -> Result<String> {
    let id = args.get("id").and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let def = registry.collections.get(slug)
        .context("Collection not found")?;

    let password = if def.is_auth_collection() {
        args.get("password").and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    } else {
        None
    };

    let mut data = HashMap::new();
    let mut join_data = HashMap::new();
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            if k == "id" || k == "password" { continue; }
            match v {
                Value::String(s) => { data.insert(k.clone(), s.clone()); }
                Value::Number(n) => { data.insert(k.clone(), n.to_string()); }
                Value::Bool(b) => { data.insert(k.clone(), if *b { "1".to_string() } else { "0".to_string() }); }
                Value::Array(_) | Value::Object(_) => { join_data.insert(k.clone(), v.clone()); }
                Value::Null => {}
            }
        }
    }

    let (doc, _ctx) = crate::service::update_document(
        pool, runner, slug, id, def, data, &join_data,
        password.as_deref(), None, None, None, false,
    )?;

    tracing::info!("MCP update {}: {}", slug, id);
    Ok(serde_json::to_string_pretty(&doc_to_json(&doc))?)
}

fn exec_delete(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
) -> Result<String> {
    let id = args.get("id").and_then(|v| v.as_str())
        .context("Missing 'id' argument")?;
    let def = registry.collections.get(slug)
        .context("Collection not found")?;

    crate::service::delete_document(pool, runner, slug, id, def, None, None)?;

    tracing::info!("MCP delete {}: {}", slug, id);
    Ok(json!({ "deleted": id }).to_string())
}

fn exec_read_global(
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
) -> Result<String> {
    let def = registry.globals.get(slug)
        .context("Global not found")?;
    let conn = pool.get().context("DB connection")?;

    match query::get_global(&conn, slug, def, None) {
        Ok(d) => Ok(serde_json::to_string_pretty(&doc_to_json(&d))?),
        Err(e) => {
            // "not found" is expected for globals that haven't been written yet
            let err_msg = e.to_string();
            if err_msg.contains("not found") || err_msg.contains("no rows") {
                Ok(json!({}).to_string())
            } else {
                Err(e).context(format!("Failed to read global '{}'", slug))
            }
        }
    }
}

fn exec_update_global(
    args: &Value,
    slug: &str,
    registry: &Arc<Registry>,
    pool: &DbPool,
    runner: &HookRunner,
) -> Result<String> {
    let def = registry.globals.get(slug)
        .context("Global not found")?;

    let mut data = HashMap::new();
    let mut join_data = HashMap::new();
    if let Some(obj) = args.as_object() {
        for (k, v) in obj {
            match v {
                Value::String(s) => { data.insert(k.clone(), s.clone()); }
                Value::Number(n) => { data.insert(k.clone(), n.to_string()); }
                Value::Bool(b) => { data.insert(k.clone(), if *b { "1".to_string() } else { "0".to_string() }); }
                Value::Array(_) | Value::Object(_) => { join_data.insert(k.clone(), v.clone()); }
                Value::Null => {}
            }
        }
    }

    let (doc, _ctx) = crate::service::update_global_document(
        pool, runner, slug, def, data, &join_data, None, None, None, false,
    )?;

    tracing::info!("MCP update global: {}", slug);
    Ok(serde_json::to_string_pretty(&doc_to_json(&doc))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::McpConfig;

    fn make_registry() -> Registry {
        let mut reg = Registry::new();
        reg.register_collection(CollectionDefinition {
            slug: "posts".to_string(),
            ..Default::default()
        });
        reg.register_collection(CollectionDefinition {
            slug: "users".to_string(),
            ..Default::default()
        });
        reg.register_global(GlobalDefinition {
            slug: "settings".to_string(),
            ..Default::default()
        });
        reg
    }

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
    fn list_field_types_returns_all_types() {
        let result = exec_list_field_types().unwrap();
        let types: Vec<Value> = serde_json::from_str(&result).unwrap();
        assert_eq!(types.len(), 20);

        // Verify all expected field types are present
        let names: Vec<&str> = types.iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for expected in &[
            "text", "number", "textarea", "select", "radio", "checkbox", "date",
            "email", "json", "richtext", "code", "relationship", "array", "group",
            "upload", "blocks", "row", "collapsible", "tabs", "join",
        ] {
            assert!(names.contains(expected), "Missing field type: {}", expected);
        }

        // Verify each entry has all required keys
        for t in &types {
            assert!(t.get("name").is_some());
            assert!(t.get("description").is_some());
            assert!(t.get("json_schema_type").is_some());
            assert!(t.get("supports_has_many").is_some());
            assert!(t.get("supports_sub_fields").is_some());
            assert!(t.get("supports_options").is_some());
        }
    }

    #[test]
    fn cli_reference_all_commands() {
        let result = exec_cli_reference(&json!({})).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        let commands = parsed["commands"].as_array().unwrap();
        assert!(commands.len() >= 15);

        let names: Vec<&str> = commands.iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        for expected in &["serve", "migrate", "user", "backup", "jobs", "mcp"] {
            assert!(names.contains(expected), "Missing command: {}", expected);
        }
    }

    #[test]
    fn cli_reference_specific_command() {
        let result = exec_cli_reference(&json!({ "command": "migrate" })).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("subcommands").is_some());
        let subs = parsed["subcommands"].as_array().unwrap();
        let sub_names: Vec<&str> = subs.iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert!(sub_names.contains(&"up"));
        assert!(sub_names.contains(&"down"));
        assert!(sub_names.contains(&"create"));
        assert!(sub_names.contains(&"list"));
        assert!(sub_names.contains(&"fresh"));
    }

    #[test]
    fn cli_reference_unknown_command() {
        let result = exec_cli_reference(&json!({ "command": "nonexistent" })).unwrap();
        let parsed: Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.get("error").is_some());
    }

    #[test]
    fn safe_config_path_rejects_absolute() {
        let dir = std::path::Path::new("/tmp");
        assert!(safe_config_path(dir, "/etc/passwd").is_err());
    }

    #[test]
    fn safe_config_path_rejects_dot_dot() {
        let dir = std::path::Path::new("/tmp");
        assert!(safe_config_path(dir, "../etc/passwd").is_err());
        assert!(safe_config_path(dir, "foo/../../etc/passwd").is_err());
    }

    #[test]
    fn safe_config_path_allows_relative() {
        let dir = std::env::temp_dir();
        // Should succeed — a simple relative path within an existing dir
        let result = safe_config_path(&dir, "test_file.txt");
        assert!(result.is_ok());
    }

    // config_tools enforcement is tested via generate_tools (excluded by default)
    // and the match guard in execute_tool. The guard is purely a config check before
    // any DB/hook access, verified by code inspection. Integration tests cover e2e.

    #[test]
    fn parse_where_in_operator() {
        let args = json!({
            "where": {
                "status": { "in": ["draft", "review"] }
            }
        });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert_eq!(f.field, "status");
                match &f.op {
                    query::FilterOp::In(vals) => assert_eq!(vals, &["draft", "review"]),
                    other => panic!("Expected In, got {:?}", other),
                }
            }
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn parse_where_not_in_operator() {
        let args = json!({
            "where": {
                "role": { "not_in": ["banned", "suspended"] }
            }
        });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert_eq!(f.field, "role");
                assert!(matches!(&f.op, query::FilterOp::NotIn(_)));
            }
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn parse_where_exists_operator() {
        let args = json!({
            "where": {
                "avatar": { "exists": true }
            }
        });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert_eq!(f.field, "avatar");
                assert!(matches!(&f.op, query::FilterOp::Exists));
            }
            other => panic!("Expected Single, got {:?}", other),
        }
    }

    #[test]
    fn parse_where_not_exists_operator() {
        let args = json!({
            "where": {
                "deleted_at": { "not_exists": true }
            }
        });
        let clauses = parse_where_filters(&args);
        assert_eq!(clauses.len(), 1);
        match &clauses[0] {
            query::FilterClause::Single(f) => {
                assert!(matches!(&f.op, query::FilterOp::NotExists));
            }
            other => panic!("Expected Single, got {:?}", other),
        }
    }
}
