//! Static (non-CRUD) tool implementations: collection listing, describe, field types,
//! CLI reference, and config file operations.

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use serde_json::{json, Value};

use crate::config::McpConfig;
use crate::core::Registry;

use super::should_include;
use super::super::schema::{CrudOp, collection_input_schema, global_input_schema};

pub(super) fn exec_list_collections(registry: &Registry, mcp_config: &McpConfig) -> Result<String> {
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

pub(super) fn exec_describe_collection(args: &Value, registry: &Registry, mcp_config: &McpConfig) -> Result<String> {
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

pub(super) fn exec_list_field_types() -> Result<String> {
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

pub(super) fn exec_cli_reference(args: &Value) -> Result<String> {
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
pub(super) fn safe_config_path(config_dir: &Path, relative: &str) -> Result<std::path::PathBuf> {
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

pub(super) fn exec_read_config_file(args: &Value, config_dir: &Path) -> Result<String> {
    let path = args.get("path").and_then(|v| v.as_str())
        .context("Missing 'path' argument")?;
    let full_path = safe_config_path(config_dir, path)?;
    let content = std::fs::read_to_string(&full_path)
        .with_context(|| format!("Failed to read {}", full_path.display()))?;
    Ok(content)
}

pub(super) fn exec_write_config_file(args: &Value, config_dir: &Path) -> Result<String> {
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

pub(super) fn exec_list_config_files(args: &Value, config_dir: &Path) -> Result<String> {
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
