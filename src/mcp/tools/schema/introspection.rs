//! Introspection tools: list collections, describe collection, field types, CLI reference.

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json, to_string_pretty};

use crate::{
    config::McpConfig,
    core::Registry,
    mcp::{
        schema::{CrudOp, collection_input_schema, global_input_schema},
        tools::should_include,
    },
};

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
    Ok(to_string_pretty(&result)?)
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

        return Ok(to_string_pretty(&result)?);
    }

    if let Some(def) = registry.globals.get(slug) {
        let schema = global_input_schema(def, CrudOp::Update);
        let result = json!({
            "slug": slug,
            "type": "global",
            "label": def.display_name(),
            "schema": schema,
        });

        return Ok(to_string_pretty(&result)?);
    }

    bail!("Unknown collection or global: {}", slug)
}

/// List all available field types with their capabilities.
pub(in crate::mcp::tools) fn exec_list_field_types() -> Result<String> {
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
    Ok(to_string_pretty(&types)?)
}

/// Return CLI reference documentation, optionally filtered by command name.
pub(in crate::mcp::tools) fn exec_cli_reference(args: &Value) -> Result<String> {
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
            Ok(to_string_pretty(&overview)?)
        }
        Some(cmd) => {
            let detail = match cmd {
                "serve" => json!({
                    "command": "crap-cms serve",
                    "description": "Start the admin UI and gRPC servers",
                    "flags": [
                        { "flag": "-d, --detach", "description": "Run in the background (detached)" }
                    ],
                    "examples": [
                        "crap-cms serve",
                        "crap-cms serve --detach"
                    ]
                }),
                "status" => json!({
                    "command": "crap-cms status",
                    "description": "Show project status (collections, globals, migrations)",
                    "examples": ["crap-cms status"]
                }),
                "init" => json!({
                    "command": "crap-cms init [DIR]",
                    "description": "Scaffold a new config directory with default structure",
                    "args": [
                        { "arg": "DIR", "description": "Directory to create (default: ./crap-cms)" }
                    ],
                    "examples": [
                        "crap-cms init",
                        "crap-cms init"
                    ]
                }),
                "make" | "make collection" | "make global" | "make hook" | "make job" => json!({
                    "command": "crap-cms make <SUBCOMMAND>",
                    "description": "Generate scaffolding files",
                    "subcommands": [
                        {
                            "name": "collection",
                            "usage": "crap-cms make collection [SLUG] [OPTIONS]",
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
                                "crap-cms make collection posts -F 'title:text:required,body:richtext,status:select'",
                                "crap-cms make collection users --auth --no-input"
                            ]
                        },
                        {
                            "name": "global",
                            "usage": "crap-cms make global [SLUG] [OPTIONS]",
                            "description": "Generate a global Lua file",
                            "flags": [
                                { "flag": "-F, --fields <FIELDS>", "description": "Inline field shorthand" },
                                { "flag": "-f, --force", "description": "Overwrite existing file" }
                            ]
                        },
                        {
                            "name": "hook",
                            "usage": "crap-cms make hook [NAME] [OPTIONS]",
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
                            "usage": "crap-cms make job [SLUG] [OPTIONS]",
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
                "blueprint" | "blueprint save" | "blueprint use" | "blueprint list"
                | "blueprint remove" => json!({
                    "command": "crap-cms blueprint <SUBCOMMAND>",
                    "description": "Manage saved blueprints",
                    "subcommands": [
                        { "name": "save", "usage": "crap-cms blueprint save <NAME> [-f]", "description": "Save a config directory as a reusable blueprint" },
                        { "name": "use", "usage": "crap-cms blueprint use [NAME] [DIR]", "description": "Create a new project from a saved blueprint" },
                        { "name": "list", "usage": "crap-cms blueprint list", "description": "List all saved blueprints" },
                        { "name": "remove", "usage": "crap-cms blueprint remove [NAME]", "description": "Remove a saved blueprint" }
                    ]
                }),
                "user"
                | "user create"
                | "user list"
                | "user delete"
                | "user lock"
                | "user unlock"
                | "user change-password" => json!({
                    "command": "crap-cms user <SUBCOMMAND>",
                    "description": "User management for auth collections",
                    "subcommands": [
                        {
                            "name": "create",
                            "usage": "crap-cms user create [OPTIONS]",
                            "description": "Create a new user",
                            "flags": [
                                { "flag": "-c, --collection <SLUG>", "description": "Auth collection slug (default: users)" },
                                { "flag": "-e, --email <EMAIL>", "description": "User email" },
                                { "flag": "-p, --password <PW>", "description": "User password (omit for interactive prompt)" },
                                { "flag": "-f, --field <KEY=VALUE>", "description": "Extra fields (repeatable)" }
                            ],
                            "examples": [
                                "crap-cms user create -e admin@example.com",
                                "crap-cms user create -e admin@example.com -p secret -f role=admin -f name='Admin'"
                            ]
                        },
                        { "name": "list", "usage": "crap-cms user list [-c <SLUG>]", "description": "List users in an auth collection" },
                        {
                            "name": "delete",
                            "usage": "crap-cms user delete [OPTIONS]",
                            "description": "Delete a user",
                            "flags": [
                                { "flag": "-e, --email <EMAIL>", "description": "User email" },
                                { "flag": "--id <ID>", "description": "User ID" },
                                { "flag": "-y, --confirm", "description": "Skip confirmation prompt" }
                            ]
                        },
                        { "name": "lock", "usage": "crap-cms user lock [-e <EMAIL>] [--id <ID>]", "description": "Lock a user account (prevent login)" },
                        { "name": "unlock", "usage": "crap-cms user unlock [-e <EMAIL>] [--id <ID>]", "description": "Unlock a user account" },
                        {
                            "name": "change-password",
                            "usage": "crap-cms user change-password [OPTIONS]",
                            "description": "Change a user's password",
                            "flags": [
                                { "flag": "-e, --email <EMAIL>", "description": "User email" },
                                { "flag": "--id <ID>", "description": "User ID" },
                                { "flag": "-p, --password <PW>", "description": "New password (omit for interactive)" }
                            ]
                        }
                    ]
                }),
                "migrate" | "migrate create" | "migrate up" | "migrate down" | "migrate list"
                | "migrate fresh" => json!({
                    "command": "crap-cms migrate <SUBCOMMAND>",
                    "description": "Run database migrations",
                    "subcommands": [
                        { "name": "create", "usage": "crap-cms migrate create <NAME>", "description": "Create a new migration file" },
                        { "name": "up", "usage": "crap-cms migrate up", "description": "Schema sync + run pending Lua data migrations" },
                        { "name": "down", "usage": "crap-cms migrate down [-s <N>]", "description": "Rollback last N data migrations (default: 1)" },
                        { "name": "list", "usage": "crap-cms migrate list", "description": "Show all migration files with applied/pending status" },
                        { "name": "fresh", "usage": "crap-cms migrate fresh -y", "description": "Drop all tables, recreate from Lua definitions, run all migrations (destructive!)" }
                    ],
                    "examples": [
                        "crap-cms migrate up",
                        "crap-cms migrate create add_categories",
                        "crap-cms migrate down -s 2",
                        "crap-cms migrate fresh -y"
                    ]
                }),
                "backup" => json!({
                    "command": "crap-cms backup [OPTIONS]",
                    "description": "Backup database and optionally uploads",
                    "flags": [
                        { "flag": "-o, --output <DIR>", "description": "Output directory (default: <config_dir>/backups/)" },
                        { "flag": "-i, --include-uploads", "description": "Also compress the uploads directory" }
                    ],
                    "examples": [
                        "crap-cms backup",
                        "crap-cms backup -o /backups -i"
                    ]
                }),
                "db" | "db console" | "db cleanup" => json!({
                    "command": "crap-cms db <SUBCOMMAND>",
                    "description": "Database tools",
                    "subcommands": [
                        { "name": "console", "usage": "crap-cms db console", "description": "Open an interactive SQLite console" },
                        {
                            "name": "cleanup",
                            "usage": "crap-cms db cleanup [--confirm]",
                            "description": "Detect and optionally remove orphan columns not in Lua definitions",
                            "flags": [
                                { "flag": "--confirm", "description": "Actually drop orphan columns (default: dry-run report)" }
                            ]
                        }
                    ]
                }),
                "export" => json!({
                    "command": "crap-cms export [OPTIONS]",
                    "description": "Export collection data to JSON",
                    "flags": [
                        { "flag": "-c, --collection <SLUG>", "description": "Export only this collection (default: all)" },
                        { "flag": "-o, --output <FILE>", "description": "Output file (default: stdout)" }
                    ],
                    "examples": [
                        "crap-cms export",
                        "crap-cms export -c posts -o posts.json"
                    ]
                }),
                "import" => json!({
                    "command": "crap-cms import <FILE> [OPTIONS]",
                    "description": "Import collection data from JSON",
                    "flags": [
                        { "flag": "-c, --collection <SLUG>", "description": "Import only this collection (default: all in file)" }
                    ],
                    "examples": [
                        "crap-cms import backup.json",
                        "crap-cms import posts.json -c posts"
                    ]
                }),
                "typegen" => json!({
                    "command": "crap-cms typegen [OPTIONS]",
                    "description": "Generate typed definitions from collection schemas",
                    "flags": [
                        { "flag": "-l, --lang <LANG>", "description": "Output language: lua, ts, go, py, rs, all (default: lua)" },
                        { "flag": "-o, --output <DIR>", "description": "Output directory (default: <config>/types/)" }
                    ],
                    "examples": [
                        "crap-cms typegen -l ts",
                        "crap-cms typegen -l all -o ./types"
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
                            "usage": "crap-cms templates extract [PATHS...] [OPTIONS]",
                            "description": "Extract default files into config directory for customization",
                            "flags": [
                                { "flag": "-a, --all", "description": "Extract all files" },
                                { "flag": "-t, --type <TYPE>", "description": "Filter: 'templates' or 'static' (only with --all)" },
                                { "flag": "-f, --force", "description": "Overwrite existing files" }
                            ]
                        }
                    ]
                }),
                "jobs" | "jobs list" | "jobs trigger" | "jobs status" | "jobs purge"
                | "jobs healthcheck" => json!({
                    "command": "crap-cms jobs <SUBCOMMAND>",
                    "description": "Manage background jobs",
                    "subcommands": [
                        { "name": "list", "usage": "crap-cms jobs list", "description": "List defined jobs and recent runs" },
                        {
                            "name": "trigger",
                            "usage": "crap-cms jobs trigger <SLUG> [OPTIONS]",
                            "description": "Trigger a job manually",
                            "flags": [
                                { "flag": "-d, --data <JSON>", "description": "JSON data to pass to the job" }
                            ]
                        },
                        {
                            "name": "status",
                            "usage": "crap-cms jobs status [OPTIONS]",
                            "description": "Show job run history",
                            "flags": [
                                { "flag": "--id <ID>", "description": "Show a single job run by ID" },
                                { "flag": "-s, --slug <SLUG>", "description": "Filter by job slug" },
                                { "flag": "-l, --limit <N>", "description": "Max results (default: 20)" }
                            ]
                        },
                        {
                            "name": "purge",
                            "usage": "crap-cms jobs purge [OPTIONS]",
                            "description": "Clean up old completed/failed job runs",
                            "flags": [
                                { "flag": "--older-than <DURATION>", "description": "Delete runs older than this (e.g., '7d', '24h'). Default: 7d" }
                            ]
                        },
                        { "name": "healthcheck", "usage": "crap-cms jobs healthcheck", "description": "Check job system health" }
                    ]
                }),
                "images" | "images list" | "images stats" | "images retry" | "images purge" => {
                    json!({
                        "command": "crap-cms images <SUBCOMMAND>",
                        "description": "Manage image processing queue",
                        "subcommands": [
                            {
                                "name": "list",
                                "usage": "crap-cms images list [OPTIONS]",
                                "description": "List image processing queue entries",
                                "flags": [
                                    { "flag": "-s, --status <STATUS>", "description": "Filter: pending, processing, completed, failed" },
                                    { "flag": "-l, --limit <N>", "description": "Max entries (default: 20)" }
                                ]
                            },
                            { "name": "stats", "usage": "crap-cms images stats", "description": "Show queue statistics by status" },
                            {
                                "name": "retry",
                                "usage": "crap-cms images retry [OPTIONS]",
                                "description": "Retry failed queue entries",
                                "flags": [
                                    { "flag": "--id <ID>", "description": "Retry a specific entry by ID" },
                                    { "flag": "--all", "description": "Retry all failed entries" },
                                    { "flag": "-y, --confirm", "description": "Confirm retry all (required with --all)" }
                                ]
                            },
                            {
                                "name": "purge",
                                "usage": "crap-cms images purge [OPTIONS]",
                                "description": "Purge old completed/failed entries",
                                "flags": [
                                    { "flag": "--older-than <DURATION>", "description": "Delete entries older than this (e.g., '7d'). Default: 7d" }
                                ]
                            }
                        ]
                    })
                }
                "mcp" => json!({
                    "command": "crap-cms mcp",
                    "description": "Start the MCP (Model Context Protocol) server using stdio transport",
                    "examples": ["crap-cms mcp"]
                }),
                _ => {
                    json!({ "error": format!("Unknown command: '{}'. Call cli_reference without a command argument to see all available commands.", cmd) })
                }
            };
            Ok(to_string_pretty(&detail)?)
        }
    }
}
