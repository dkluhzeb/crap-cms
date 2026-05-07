//! Introspection tools: list collections, describe collection, field types, CLI reference.

use anyhow::{Context as _, Result, bail};
use serde::Serialize;
use serde_json::{Value, to_string_pretty};

use crate::{
    config::McpConfig,
    core::Registry,
    mcp::{
        schema::{CrudOp, collection_input_schema, global_input_schema},
        tools::should_include,
    },
};

/// Single entry in the `list_field_types` MCP tool response.
#[derive(Serialize)]
struct FieldTypeInfo {
    name: &'static str,
    description: &'static str,
    json_schema_type: &'static str,
    supports_has_many: bool,
    supports_sub_fields: bool,
    supports_options: bool,
}

const FIELD_TYPES: &[FieldTypeInfo] = &[
    FieldTypeInfo {
        name: "text",
        description: "Single-line text input",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "number",
        description: "Numeric input (integer or float)",
        json_schema_type: "number",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "textarea",
        description: "Multi-line text input",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "select",
        description: "Dropdown select from predefined options",
        json_schema_type: "string",
        supports_has_many: true,
        supports_sub_fields: false,
        supports_options: true,
    },
    FieldTypeInfo {
        name: "radio",
        description: "Radio button group from predefined options",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: true,
    },
    FieldTypeInfo {
        name: "checkbox",
        description: "Boolean checkbox (true/false)",
        json_schema_type: "boolean",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "date",
        description: "Date/datetime picker",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "email",
        description: "Email address input with validation",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "json",
        description: "Raw JSON data stored as text",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "richtext",
        description: "Rich text editor (HTML content)",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "code",
        description: "Code editor with syntax highlighting",
        json_schema_type: "string",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "relationship",
        description: "Reference to document(s) in another collection",
        json_schema_type: "string",
        supports_has_many: true,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "array",
        description: "Repeatable group of sub-fields (stored in join table)",
        json_schema_type: "array",
        supports_has_many: false,
        supports_sub_fields: true,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "group",
        description: "Named group of sub-fields (columns prefixed with group name)",
        json_schema_type: "object",
        supports_has_many: false,
        supports_sub_fields: true,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "upload",
        description: "File upload field referencing an upload collection",
        json_schema_type: "string",
        supports_has_many: true,
        supports_sub_fields: false,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "blocks",
        description: "Flexible content blocks with different block types",
        json_schema_type: "array",
        supports_has_many: false,
        supports_sub_fields: true,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "row",
        description: "Layout-only horizontal container. Sub-fields promoted to parent level (no prefix)",
        json_schema_type: "null",
        supports_has_many: false,
        supports_sub_fields: true,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "collapsible",
        description: "Layout-only collapsible container. Sub-fields promoted to parent level (no prefix)",
        json_schema_type: "null",
        supports_has_many: false,
        supports_sub_fields: true,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "tabs",
        description: "Layout-only tabbed container. Sub-fields promoted to parent level (no prefix)",
        json_schema_type: "null",
        supports_has_many: false,
        supports_sub_fields: true,
        supports_options: false,
    },
    FieldTypeInfo {
        name: "join",
        description: "Virtual reverse-relationship field. Shows documents from another collection that reference this document. No stored data.",
        json_schema_type: "null",
        supports_has_many: false,
        supports_sub_fields: false,
        supports_options: false,
    },
];

/// Top-level shape returned when `cli_reference` is called without a command.
#[derive(Serialize)]
struct CliOverview {
    binary: &'static str,
    description: &'static str,
    usage: &'static str,
    commands: &'static [CliCommandSummary],
}

#[derive(Serialize)]
struct CliCommandSummary {
    name: &'static str,
    description: &'static str,
}

/// Shape returned when `cli_reference` is called with a specific command name.
/// All optional fields are skipped when `None` to preserve the existing wire
/// format exactly (some commands have no subcommands, etc.).
#[derive(Serialize)]
struct CliCommandDetail {
    command: &'static str,
    description: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    flags: Option<&'static [CliFlag]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    args: Option<&'static [CliArg]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subcommands: Option<&'static [CliSubcommand]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    examples: Option<&'static [&'static str]>,
}

#[derive(Serialize)]
struct CliFlag {
    flag: &'static str,
    description: &'static str,
}

#[derive(Serialize)]
struct CliArg {
    arg: &'static str,
    description: &'static str,
}

#[derive(Serialize)]
struct CliSubcommand {
    name: &'static str,
    usage: &'static str,
    description: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    flags: Option<&'static [CliFlag]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    examples: Option<&'static [&'static str]>,
}

#[derive(Serialize)]
struct CliReferenceError {
    error: String,
}

static CLI_COMMANDS_OVERVIEW: &[CliCommandSummary] = &[
    CliCommandSummary {
        name: "serve",
        description: "Start the admin UI and gRPC servers",
    },
    CliCommandSummary {
        name: "status",
        description: "Show project status (collections, globals, migrations)",
    },
    CliCommandSummary {
        name: "init",
        description: "Scaffold a new config directory",
    },
    CliCommandSummary {
        name: "make",
        description: "Generate scaffolding files (collection, global, hook, job)",
    },
    CliCommandSummary {
        name: "blueprint",
        description: "Manage saved blueprints (save, use, list, remove)",
    },
    CliCommandSummary {
        name: "user",
        description: "User management for auth collections (create, list, delete, lock, unlock, change-password)",
    },
    CliCommandSummary {
        name: "migrate",
        description: "Run database migrations (create, up, down, list, fresh)",
    },
    CliCommandSummary {
        name: "backup",
        description: "Backup database and optionally uploads",
    },
    CliCommandSummary {
        name: "db",
        description: "Database tools (console, cleanup)",
    },
    CliCommandSummary {
        name: "export",
        description: "Export collection data to JSON",
    },
    CliCommandSummary {
        name: "import",
        description: "Import collection data from JSON",
    },
    CliCommandSummary {
        name: "typegen",
        description: "Generate typed definitions from collection schemas",
    },
    CliCommandSummary {
        name: "proto",
        description: "Export the embedded content.proto file",
    },
    CliCommandSummary {
        name: "templates",
        description: "List and extract default admin templates and static files",
    },
    CliCommandSummary {
        name: "jobs",
        description: "Manage background jobs (list, trigger, status, purge, healthcheck)",
    },
    CliCommandSummary {
        name: "images",
        description: "Manage image processing queue (list, stats, retry, purge)",
    },
    CliCommandSummary {
        name: "trash",
        description: "Manage soft-deleted documents (list, restore, purge, empty)",
    },
    CliCommandSummary {
        name: "mcp",
        description: "Start the MCP (Model Context Protocol) server (stdio transport)",
    },
    CliCommandSummary {
        name: "logs",
        description: "View and manage log files",
    },
    CliCommandSummary {
        name: "work",
        description: "Run a standalone job worker (without HTTP/gRPC servers)",
    },
    CliCommandSummary {
        name: "bench",
        description: "Benchmark hooks, queries, and write cycles",
    },
    CliCommandSummary {
        name: "update",
        description: "Manage installed versions of crap-cms (install, use, check, completions)",
    },
    CliCommandSummary {
        name: "restore",
        description: "Restore database (and optionally uploads) from a backup",
    },
];

static CLI_DETAIL_SERVE: CliCommandDetail = CliCommandDetail {
    command: "crap-cms serve",
    description: "Start the admin UI and gRPC servers",
    flags: Some(&[CliFlag {
        flag: "-d, --detach",
        description: "Run in the background (detached)",
    }]),
    args: None,
    subcommands: None,
    examples: Some(&["crap-cms serve", "crap-cms serve --detach"]),
};

static CLI_DETAIL_STATUS: CliCommandDetail = CliCommandDetail {
    command: "crap-cms status [--check]",
    description: "Show project status (server config, collections with row/trash counts, globals, versioning, access rules, hooks, live events, migrations, jobs). With --check, runs a 24-rule best-practice audit.",
    flags: Some(&[CliFlag {
        flag: "--check",
        description: "Run best-practice health checks on configuration and project state",
    }]),
    args: None,
    subcommands: None,
    examples: Some(&["crap-cms status", "crap-cms status --check"]),
};

static CLI_DETAIL_INIT: CliCommandDetail = CliCommandDetail {
    command: "crap-cms init [DIR]",
    description: "Scaffold a new config directory with default structure",
    flags: None,
    args: Some(&[CliArg {
        arg: "DIR",
        description: "Directory to create (default: ./crap-cms)",
    }]),
    subcommands: None,
    examples: Some(&["crap-cms init", "crap-cms init"]),
};

static CLI_DETAIL_MAKE: CliCommandDetail = CliCommandDetail {
    command: "crap-cms make <SUBCOMMAND>",
    description: "Generate scaffolding files",
    flags: None,
    args: None,
    subcommands: Some(&[
        CliSubcommand {
            name: "collection",
            usage: "crap-cms make collection [SLUG] [OPTIONS]",
            description: "Generate a collection Lua file",
            flags: Some(&[
                CliFlag {
                    flag: "-F, --fields <FIELDS>",
                    description: "Inline field shorthand (e.g., 'title:text:required,status:select')",
                },
                CliFlag {
                    flag: "-T, --no-timestamps",
                    description: "Disable timestamps",
                },
                CliFlag {
                    flag: "--auth",
                    description: "Enable auth (email/password login)",
                },
                CliFlag {
                    flag: "--upload",
                    description: "Enable uploads (file upload collection)",
                },
                CliFlag {
                    flag: "--versions",
                    description: "Enable versioning (draft/publish)",
                },
                CliFlag {
                    flag: "--no-input",
                    description: "Non-interactive mode",
                },
                CliFlag {
                    flag: "-f, --force",
                    description: "Overwrite existing file",
                },
            ]),
            examples: Some(&[
                "crap-cms make collection posts -F 'title:text:required,body:richtext,status:select'",
                "crap-cms make collection users --auth --no-input",
            ]),
        },
        CliSubcommand {
            name: "global",
            usage: "crap-cms make global [SLUG] [OPTIONS]",
            description: "Generate a global Lua file",
            flags: Some(&[
                CliFlag {
                    flag: "-F, --fields <FIELDS>",
                    description: "Inline field shorthand",
                },
                CliFlag {
                    flag: "-f, --force",
                    description: "Overwrite existing file",
                },
            ]),
            examples: None,
        },
        CliSubcommand {
            name: "hook",
            usage: "crap-cms make hook [NAME] [OPTIONS]",
            description: "Generate a hook file",
            flags: Some(&[
                CliFlag {
                    flag: "-t, --type <TYPE>",
                    description: "Hook type: collection, field, or access",
                },
                CliFlag {
                    flag: "-c, --collection <SLUG>",
                    description: "Target collection slug",
                },
                CliFlag {
                    flag: "-l, --position <POS>",
                    description: "Lifecycle position (e.g., before_change, after_read)",
                },
                CliFlag {
                    flag: "-F, --field <NAME>",
                    description: "Target field name (field hooks only)",
                },
                CliFlag {
                    flag: "--force",
                    description: "Overwrite existing file",
                },
            ]),
            examples: None,
        },
        CliSubcommand {
            name: "job",
            usage: "crap-cms make job [SLUG] [OPTIONS]",
            description: "Generate a job Lua file",
            flags: Some(&[
                CliFlag {
                    flag: "-s, --schedule <CRON>",
                    description: "Cron schedule expression",
                },
                CliFlag {
                    flag: "-q, --queue <NAME>",
                    description: "Queue name (default: 'default')",
                },
                CliFlag {
                    flag: "-r, --retries <N>",
                    description: "Max retry attempts (default: 0)",
                },
                CliFlag {
                    flag: "-t, --timeout <SECS>",
                    description: "Timeout in seconds (default: 60)",
                },
                CliFlag {
                    flag: "-f, --force",
                    description: "Overwrite existing file",
                },
            ]),
            examples: None,
        },
    ]),
    examples: None,
};

static CLI_DETAIL_BLUEPRINT: CliCommandDetail = CliCommandDetail {
    command: "crap-cms blueprint <SUBCOMMAND>",
    description: "Manage saved blueprints",
    flags: None,
    args: None,
    subcommands: Some(&[
        CliSubcommand {
            name: "save",
            usage: "crap-cms blueprint save <NAME> [-f]",
            description: "Save a config directory as a reusable blueprint",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "use",
            usage: "crap-cms blueprint use [NAME] [DIR]",
            description: "Create a new project from a saved blueprint",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "list",
            usage: "crap-cms blueprint list",
            description: "List all saved blueprints",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "remove",
            usage: "crap-cms blueprint remove [NAME]",
            description: "Remove a saved blueprint",
            flags: None,
            examples: None,
        },
    ]),
    examples: None,
};

static CLI_DETAIL_USER: CliCommandDetail = CliCommandDetail {
    command: "crap-cms user <SUBCOMMAND>",
    description: "User management for auth collections",
    flags: None,
    args: None,
    subcommands: Some(&[
        CliSubcommand {
            name: "create",
            usage: "crap-cms user create [OPTIONS]",
            description: "Create a new user",
            flags: Some(&[
                CliFlag {
                    flag: "-c, --collection <SLUG>",
                    description: "Auth collection slug (default: users)",
                },
                CliFlag {
                    flag: "-e, --email <EMAIL>",
                    description: "User email",
                },
                CliFlag {
                    flag: "-p, --password <PW>",
                    description: "User password (omit for interactive prompt)",
                },
                CliFlag {
                    flag: "-f, --field <KEY=VALUE>",
                    description: "Extra fields (repeatable)",
                },
            ]),
            examples: Some(&[
                "crap-cms user create -e admin@example.com",
                "crap-cms user create -e admin@example.com -p secret -f role=admin -f name='Admin'",
            ]),
        },
        CliSubcommand {
            name: "list",
            usage: "crap-cms user list [-c <SLUG>]",
            description: "List users in an auth collection",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "delete",
            usage: "crap-cms user delete [OPTIONS]",
            description: "Delete a user",
            flags: Some(&[
                CliFlag {
                    flag: "-e, --email <EMAIL>",
                    description: "User email",
                },
                CliFlag {
                    flag: "--id <ID>",
                    description: "User ID",
                },
                CliFlag {
                    flag: "-y, --confirm",
                    description: "Skip confirmation prompt",
                },
            ]),
            examples: None,
        },
        CliSubcommand {
            name: "lock",
            usage: "crap-cms user lock [-e <EMAIL>] [--id <ID>]",
            description: "Lock a user account (prevent login)",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "unlock",
            usage: "crap-cms user unlock [-e <EMAIL>] [--id <ID>]",
            description: "Unlock a user account",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "change-password",
            usage: "crap-cms user change-password [OPTIONS]",
            description: "Change a user's password",
            flags: Some(&[
                CliFlag {
                    flag: "-e, --email <EMAIL>",
                    description: "User email",
                },
                CliFlag {
                    flag: "--id <ID>",
                    description: "User ID",
                },
                CliFlag {
                    flag: "-p, --password <PW>",
                    description: "New password (omit for interactive)",
                },
            ]),
            examples: None,
        },
    ]),
    examples: None,
};

static CLI_DETAIL_MIGRATE: CliCommandDetail = CliCommandDetail {
    command: "crap-cms migrate <SUBCOMMAND>",
    description: "Run database migrations",
    flags: None,
    args: None,
    subcommands: Some(&[
        CliSubcommand {
            name: "create",
            usage: "crap-cms migrate create <NAME>",
            description: "Create a new migration file",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "up",
            usage: "crap-cms migrate up",
            description: "Schema sync + run pending Lua data migrations",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "down",
            usage: "crap-cms migrate down [-s <N>]",
            description: "Rollback last N data migrations (default: 1)",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "list",
            usage: "crap-cms migrate list",
            description: "Show all migration files with applied/pending status",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "fresh",
            usage: "crap-cms migrate fresh -y",
            description: "Drop all tables, recreate from Lua definitions, run all migrations (destructive!)",
            flags: None,
            examples: None,
        },
    ]),
    examples: Some(&[
        "crap-cms migrate up",
        "crap-cms migrate create add_categories",
        "crap-cms migrate down -s 2",
        "crap-cms migrate fresh -y",
    ]),
};

static CLI_DETAIL_BACKUP: CliCommandDetail = CliCommandDetail {
    command: "crap-cms backup [OPTIONS]",
    description: "Backup database and optionally uploads",
    flags: Some(&[
        CliFlag {
            flag: "-o, --output <DIR>",
            description: "Output directory (default: <config_dir>/backups/)",
        },
        CliFlag {
            flag: "-i, --include-uploads",
            description: "Also compress the uploads directory",
        },
    ]),
    args: None,
    subcommands: None,
    examples: Some(&["crap-cms backup", "crap-cms backup -o /backups -i"]),
};

static CLI_DETAIL_DB: CliCommandDetail = CliCommandDetail {
    command: "crap-cms db <SUBCOMMAND>",
    description: "Database tools",
    flags: None,
    args: None,
    subcommands: Some(&[
        CliSubcommand {
            name: "console",
            usage: "crap-cms db console",
            description: "Open an interactive SQLite console",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "cleanup",
            usage: "crap-cms db cleanup [--confirm]",
            description: "Detect and optionally remove orphan columns not in Lua definitions",
            flags: Some(&[CliFlag {
                flag: "--confirm",
                description: "Actually drop orphan columns (default: dry-run report)",
            }]),
            examples: None,
        },
    ]),
    examples: None,
};

static CLI_DETAIL_EXPORT: CliCommandDetail = CliCommandDetail {
    command: "crap-cms export [OPTIONS]",
    description: "Export collection data to JSON",
    flags: Some(&[
        CliFlag {
            flag: "-c, --collection <SLUG>",
            description: "Export only this collection (default: all)",
        },
        CliFlag {
            flag: "-o, --output <FILE>",
            description: "Output file (default: stdout)",
        },
    ]),
    args: None,
    subcommands: None,
    examples: Some(&["crap-cms export", "crap-cms export -c posts -o posts.json"]),
};

static CLI_DETAIL_IMPORT: CliCommandDetail = CliCommandDetail {
    command: "crap-cms import <FILE> [OPTIONS]",
    description: "Import collection data from JSON",
    flags: Some(&[CliFlag {
        flag: "-c, --collection <SLUG>",
        description: "Import only this collection (default: all in file)",
    }]),
    args: None,
    subcommands: None,
    examples: Some(&[
        "crap-cms import backup.json",
        "crap-cms import posts.json -c posts",
    ]),
};

static CLI_DETAIL_TYPEGEN: CliCommandDetail = CliCommandDetail {
    command: "crap-cms typegen [OPTIONS]",
    description: "Generate typed definitions from collection schemas",
    flags: Some(&[
        CliFlag {
            flag: "-l, --lang <LANG>",
            description: "Output language: lua, ts, go, py, rs, all (default: lua)",
        },
        CliFlag {
            flag: "-o, --output <DIR>",
            description: "Output directory (default: <config>/types/)",
        },
    ]),
    args: None,
    subcommands: None,
    examples: Some(&[
        "crap-cms typegen -l ts",
        "crap-cms typegen -l all -o ./types",
    ]),
};

static CLI_DETAIL_PROTO: CliCommandDetail = CliCommandDetail {
    command: "crap-cms proto [OPTIONS]",
    description: "Export the embedded content.proto file for gRPC client codegen",
    flags: Some(&[CliFlag {
        flag: "-o, --output <PATH>",
        description: "Output path (file or directory). Omit to write to stdout.",
    }]),
    args: None,
    subcommands: None,
    examples: Some(&["crap-cms proto", "crap-cms proto -o ./proto/content.proto"]),
};

static CLI_DETAIL_TEMPLATES: CliCommandDetail = CliCommandDetail {
    command: "crap-cms templates <SUBCOMMAND>",
    description: "List and extract default admin templates and static files",
    flags: None,
    args: None,
    subcommands: Some(&[
        CliSubcommand {
            name: "list",
            usage: "crap-cms templates list [OPTIONS]",
            description: "List all available default templates and static files",
            flags: Some(&[
                CliFlag {
                    flag: "-t, --type <TYPE>",
                    description: "Filter: 'templates' or 'static' (default: both)",
                },
                CliFlag {
                    flag: "-v, --verbose",
                    description: "Show full file tree with sizes",
                },
            ]),
            examples: None,
        },
        CliSubcommand {
            name: "extract",
            usage: "crap-cms templates extract [PATHS...] [OPTIONS]",
            description: "Extract default files into config directory for customization",
            flags: Some(&[
                CliFlag {
                    flag: "-a, --all",
                    description: "Extract all files",
                },
                CliFlag {
                    flag: "-t, --type <TYPE>",
                    description: "Filter: 'templates' or 'static' (only with --all)",
                },
                CliFlag {
                    flag: "-f, --force",
                    description: "Overwrite existing files",
                },
            ]),
            examples: None,
        },
    ]),
    examples: None,
};

static CLI_DETAIL_JOBS: CliCommandDetail = CliCommandDetail {
    command: "crap-cms jobs <SUBCOMMAND>",
    description: "Manage background jobs",
    flags: None,
    args: None,
    subcommands: Some(&[
        CliSubcommand {
            name: "list",
            usage: "crap-cms jobs list",
            description: "List defined jobs and recent runs",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "trigger",
            usage: "crap-cms jobs trigger <SLUG> [OPTIONS]",
            description: "Trigger a job manually",
            flags: Some(&[CliFlag {
                flag: "-d, --data <JSON>",
                description: "JSON data to pass to the job",
            }]),
            examples: None,
        },
        CliSubcommand {
            name: "status",
            usage: "crap-cms jobs status [OPTIONS]",
            description: "Show job run history",
            flags: Some(&[
                CliFlag {
                    flag: "--id <ID>",
                    description: "Show a single job run by ID",
                },
                CliFlag {
                    flag: "-s, --slug <SLUG>",
                    description: "Filter by job slug",
                },
                CliFlag {
                    flag: "-l, --limit <N>",
                    description: "Max results (default: 20)",
                },
            ]),
            examples: None,
        },
        CliSubcommand {
            name: "purge",
            usage: "crap-cms jobs purge [OPTIONS]",
            description: "Clean up old completed/failed job runs",
            flags: Some(&[CliFlag {
                flag: "--older-than <DURATION>",
                description: "Delete runs older than this (e.g., '7d', '24h'). Default: 7d",
            }]),
            examples: None,
        },
        CliSubcommand {
            name: "healthcheck",
            usage: "crap-cms jobs healthcheck",
            description: "Check job system health",
            flags: None,
            examples: None,
        },
    ]),
    examples: None,
};

static CLI_DETAIL_IMAGES: CliCommandDetail = CliCommandDetail {
    command: "crap-cms images <SUBCOMMAND>",
    description: "Manage image processing queue",
    flags: None,
    args: None,
    subcommands: Some(&[
        CliSubcommand {
            name: "list",
            usage: "crap-cms images list [OPTIONS]",
            description: "List image processing queue entries",
            flags: Some(&[
                CliFlag {
                    flag: "-s, --status <STATUS>",
                    description: "Filter: pending, processing, completed, failed",
                },
                CliFlag {
                    flag: "-l, --limit <N>",
                    description: "Max entries (default: 20)",
                },
            ]),
            examples: None,
        },
        CliSubcommand {
            name: "stats",
            usage: "crap-cms images stats",
            description: "Show queue statistics by status",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "retry",
            usage: "crap-cms images retry [OPTIONS]",
            description: "Retry failed queue entries",
            flags: Some(&[
                CliFlag {
                    flag: "--id <ID>",
                    description: "Retry a specific entry by ID",
                },
                CliFlag {
                    flag: "--all",
                    description: "Retry all failed entries",
                },
                CliFlag {
                    flag: "-y, --confirm",
                    description: "Confirm retry all (required with --all)",
                },
            ]),
            examples: None,
        },
        CliSubcommand {
            name: "purge",
            usage: "crap-cms images purge [OPTIONS]",
            description: "Purge old completed/failed entries",
            flags: Some(&[CliFlag {
                flag: "--older-than <DURATION>",
                description: "Delete entries older than this (e.g., '7d'). Default: 7d",
            }]),
            examples: None,
        },
    ]),
    examples: None,
};

static CLI_DETAIL_TRASH: CliCommandDetail = CliCommandDetail {
    command: "crap-cms trash <SUBCOMMAND>",
    description: "Manage soft-deleted documents",
    flags: None,
    args: None,
    subcommands: Some(&[
        CliSubcommand {
            name: "list",
            usage: "crap-cms trash list [-c <COLLECTION>]",
            description: "List trashed documents",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "restore",
            usage: "crap-cms trash restore <COLLECTION> <ID>",
            description: "Restore a trashed document",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "purge",
            usage: "crap-cms trash purge [OPTIONS]",
            description: "Permanently delete trashed documents",
            flags: Some(&[
                CliFlag {
                    flag: "-c, --collection <SLUG>",
                    description: "Filter by collection",
                },
                CliFlag {
                    flag: "--older-than <DURATION>",
                    description: "Delete docs older than this (e.g., '30d'). Default: all",
                },
                CliFlag {
                    flag: "--dry-run",
                    description: "Print what would be deleted without deleting",
                },
            ]),
            examples: None,
        },
        CliSubcommand {
            name: "empty",
            usage: "crap-cms trash empty <COLLECTION> -y",
            description: "Permanently delete all trash in a collection (requires -y)",
            flags: None,
            examples: None,
        },
    ]),
    examples: None,
};

static CLI_DETAIL_MCP: CliCommandDetail = CliCommandDetail {
    command: "crap-cms mcp",
    description: "Start the MCP (Model Context Protocol) server using stdio transport",
    flags: None,
    args: None,
    subcommands: None,
    examples: Some(&["crap-cms mcp"]),
};

static CLI_DETAIL_LOGS: CliCommandDetail = CliCommandDetail {
    command: "crap-cms logs [OPTIONS]",
    description: "View and manage log files",
    flags: Some(&[
        CliFlag {
            flag: "-f, --follow",
            description: "Follow log output in real time",
        },
        CliFlag {
            flag: "-n, --lines <N>",
            description: "Number of lines to show (default: 100)",
        },
    ]),
    args: None,
    subcommands: Some(&[CliSubcommand {
        name: "clear",
        usage: "crap-cms logs clear",
        description: "Remove old rotated log files",
        flags: None,
        examples: None,
    }]),
    examples: Some(&["crap-cms logs", "crap-cms logs -f", "crap-cms logs clear"]),
};

static CLI_DETAIL_WORK: CliCommandDetail = CliCommandDetail {
    command: "crap-cms work [OPTIONS]",
    description: "Run a standalone job worker (processes queues without HTTP/gRPC servers)",
    flags: Some(&[
        CliFlag {
            flag: "-d, --detach",
            description: "Run in the background",
        },
        CliFlag {
            flag: "--stop",
            description: "Stop a running detached worker",
        },
        CliFlag {
            flag: "--restart",
            description: "Restart a running detached worker",
        },
        CliFlag {
            flag: "--status",
            description: "Show whether a detached worker is running",
        },
        CliFlag {
            flag: "--queues <LIST>",
            description: "Comma-separated queue names (default: all)",
        },
        CliFlag {
            flag: "--concurrency <N>",
            description: "Override max concurrent jobs",
        },
        CliFlag {
            flag: "--no-cron",
            description: "Skip cron scheduling",
        },
    ]),
    args: None,
    subcommands: None,
    examples: Some(&[
        "crap-cms work",
        "crap-cms work --queues email",
        "crap-cms work -d --queues heavy --concurrency 2",
    ]),
};

static CLI_DETAIL_RESTORE: CliCommandDetail = CliCommandDetail {
    command: "crap-cms restore <BACKUP> [OPTIONS]",
    description: "Restore database (and optionally uploads) from a backup directory",
    flags: Some(&[
        CliFlag {
            flag: "-i, --include-uploads",
            description: "Also restore uploads from uploads.tar.gz",
        },
        CliFlag {
            flag: "-y, --confirm",
            description: "Required — confirms the destructive operation",
        },
    ]),
    args: None,
    subcommands: None,
    examples: Some(&[
        "crap-cms restore ./backups/backup-2026-03-07T10-00-00 -y",
        "crap-cms restore /tmp/backup -i -y",
    ]),
};

static CLI_DETAIL_BENCH: CliCommandDetail = CliCommandDetail {
    command: "crap-cms bench <SUBCOMMAND>",
    description: "Benchmark hooks, queries, and write cycles",
    flags: None,
    args: None,
    subcommands: Some(&[
        CliSubcommand {
            name: "hooks",
            usage: "crap-cms bench hooks [OPTIONS]",
            description: "Time individual Lua hooks (interactive selection by default)",
            flags: Some(&[
                CliFlag {
                    flag: "-c, --collection <SLUG>",
                    description: "Filter to a specific collection",
                },
                CliFlag {
                    flag: "-n, --iterations <N>",
                    description: "Iterations per hook (default: 10)",
                },
                CliFlag {
                    flag: "--hooks <LIST>",
                    description: "Comma-separated hook refs to run",
                },
                CliFlag {
                    flag: "--exclude <LIST>",
                    description: "Comma-separated hook refs to skip",
                },
                CliFlag {
                    flag: "--all",
                    description: "Run all hooks (skip wizard)",
                },
                CliFlag {
                    flag: "-d, --data <JSON>",
                    description: "Input data as JSON object",
                },
            ]),
            examples: None,
        },
        CliSubcommand {
            name: "queries",
            usage: "crap-cms bench queries [OPTIONS]",
            description: "Time find queries on each collection",
            flags: Some(&[
                CliFlag {
                    flag: "-c, --collection <SLUG>",
                    description: "Filter to a specific collection",
                },
                CliFlag {
                    flag: "--explain",
                    description: "Show EXPLAIN QUERY PLAN (SQLite)",
                },
                CliFlag {
                    flag: "-w, --where <JSON>",
                    description: "JSON filter clause (same format as gRPC where)",
                },
            ]),
            examples: None,
        },
        CliSubcommand {
            name: "create",
            usage: "crap-cms bench create <COLLECTION> [OPTIONS]",
            description: "Time a full create cycle (transaction rolled back)",
            flags: Some(&[
                CliFlag {
                    flag: "-n, --iterations <N>",
                    description: "Iterations (default: 5)",
                },
                CliFlag {
                    flag: "-d, --data <JSON>",
                    description: "Input data as JSON object",
                },
                CliFlag {
                    flag: "--no-hooks",
                    description: "Skip hooks (pure validation + persist)",
                },
                CliFlag {
                    flag: "-y, --yes",
                    description: "Skip confirmation prompt",
                },
            ]),
            examples: None,
        },
    ]),
    examples: Some(&[
        "crap-cms bench hooks --all",
        "crap-cms bench queries --explain",
        "crap-cms bench queries -c posts --where '{\"status\": \"published\"}' --explain",
        "crap-cms bench create posts -y -n 20",
    ]),
};

static CLI_DETAIL_UPDATE: CliCommandDetail = CliCommandDetail {
    command: "crap-cms update [SUBCOMMAND]",
    description: "Manage installed versions of crap-cms. Without a subcommand, installs latest + activates it.",
    flags: Some(&[
        CliFlag {
            flag: "-y, --yes",
            description: "Skip confirmation prompts",
        },
        CliFlag {
            flag: "--force",
            description: "Allow self-update when binary looks distro-managed",
        },
    ]),
    args: None,
    subcommands: Some(&[
        CliSubcommand {
            name: "check",
            usage: "crap-cms update check",
            description: "Compare current version to latest release",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "list",
            usage: "crap-cms update list",
            description: "List available release tags",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "install",
            usage: "crap-cms update install <VERSION>",
            description: "Download + verify + stage a version",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "use",
            usage: "crap-cms update use <VERSION>",
            description: "Switch to an installed version",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "uninstall",
            usage: "crap-cms update uninstall <VERSION>",
            description: "Remove an installed version",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "where",
            usage: "crap-cms update where",
            description: "Print path of active binary",
            flags: None,
            examples: None,
        },
        CliSubcommand {
            name: "completions",
            usage: "crap-cms update completions <SHELL> [--uninstall]",
            description: "Print shell completions to stdout; --uninstall removes installed file(s). Auto-installed after `update use` and bare `update` (bash/zsh/fish).",
            flags: None,
            examples: None,
        },
    ]),
    examples: Some(&[
        "crap-cms update",
        "crap-cms update check",
        "crap-cms update install v0.1.0-alpha.7",
        "crap-cms update use v0.1.0-alpha.7",
        "crap-cms update completions bash",
        "crap-cms update completions --uninstall",
    ]),
};

/// One entry in the `list_collections` MCP tool response.
///
/// Untagged sum: collection entries omit `type`; global entries set
/// `type: "global"`. Wire-format-compatible with prior releases.
#[derive(Serialize)]
#[serde(untagged)]
enum ListEntry<'a> {
    Collection {
        slug: &'a str,
        label: String,
        fields: usize,
        has_auth: bool,
        has_upload: bool,
        has_drafts: bool,
    },
    Global {
        slug: &'a str,
        label: String,
        #[serde(rename = "type")]
        kind: &'static str,
        fields: usize,
    },
}

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
        result.push(ListEntry::Collection {
            slug,
            label: def.display_name().to_string(),
            fields: def.fields.len(),
            has_auth: def.is_auth_collection(),
            has_upload: def.is_upload_collection(),
            has_drafts: def.has_drafts(),
        });
    }
    for (slug, def) in &registry.globals {
        result.push(ListEntry::Global {
            slug,
            label: def.display_name().to_string(),
            kind: "global",
            fields: def.fields.len(),
        });
    }
    Ok(to_string_pretty(&result)?)
}

/// Response shape for the `describe_collection` MCP tool. Internally
/// tagged on the `type` discriminator (`"collection"` or `"global"`).
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DescribeResponse<'a> {
    Collection {
        slug: &'a str,
        label: String,
        timestamps: bool,
        has_auth: bool,
        has_upload: bool,
        has_drafts: bool,
        schema: Value,
    },
    Global {
        slug: &'a str,
        label: String,
        schema: Value,
    },
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
        let response = DescribeResponse::Collection {
            slug,
            label: def.display_name().to_string(),
            timestamps: def.timestamps,
            has_auth: def.is_auth_collection(),
            has_upload: def.is_upload_collection(),
            has_drafts: def.has_drafts(),
            schema: collection_input_schema(def, CrudOp::Create),
        };

        return Ok(to_string_pretty(&response)?);
    }

    if let Some(def) = registry.globals.get(slug) {
        let response = DescribeResponse::Global {
            slug,
            label: def.display_name().to_string(),
            schema: global_input_schema(def, CrudOp::Update),
        };

        return Ok(to_string_pretty(&response)?);
    }

    bail!("Unknown collection or global: {}", slug)
}

/// List all available field types with their capabilities.
pub(in crate::mcp::tools) fn exec_list_field_types() -> Result<String> {
    Ok(to_string_pretty(FIELD_TYPES)?)
}

/// Return CLI reference documentation, optionally filtered by command name.
pub(in crate::mcp::tools) fn exec_cli_reference(command: Option<&str>) -> Result<String> {
    match command {
        None => {
            let overview = CliOverview {
                binary: "crap-cms",
                description: "Crap CMS - Headless CMS with Lua hooks",
                usage: "crap-cms <COMMAND> [OPTIONS]",
                commands: CLI_COMMANDS_OVERVIEW,
            };
            Ok(to_string_pretty(&overview)?)
        }
        Some(cmd) => {
            let detail = match cmd {
                "serve" => &CLI_DETAIL_SERVE,
                "status" => &CLI_DETAIL_STATUS,
                "init" => &CLI_DETAIL_INIT,
                "make" | "make collection" | "make global" | "make hook" | "make job" => {
                    &CLI_DETAIL_MAKE
                }
                "blueprint" | "blueprint save" | "blueprint use" | "blueprint list"
                | "blueprint remove" => &CLI_DETAIL_BLUEPRINT,
                "user"
                | "user create"
                | "user list"
                | "user delete"
                | "user lock"
                | "user unlock"
                | "user change-password" => &CLI_DETAIL_USER,
                "migrate" | "migrate create" | "migrate up" | "migrate down" | "migrate list"
                | "migrate fresh" => &CLI_DETAIL_MIGRATE,
                "backup" => &CLI_DETAIL_BACKUP,
                "db" | "db console" | "db cleanup" => &CLI_DETAIL_DB,
                "export" => &CLI_DETAIL_EXPORT,
                "import" => &CLI_DETAIL_IMPORT,
                "typegen" => &CLI_DETAIL_TYPEGEN,
                "proto" => &CLI_DETAIL_PROTO,
                "templates" | "templates list" | "templates extract" => &CLI_DETAIL_TEMPLATES,
                "jobs" | "jobs list" | "jobs trigger" | "jobs status" | "jobs purge"
                | "jobs healthcheck" => &CLI_DETAIL_JOBS,
                "images" | "images list" | "images stats" | "images retry" | "images purge" => {
                    &CLI_DETAIL_IMAGES
                }
                "trash" | "trash list" | "trash restore" | "trash purge" | "trash empty" => {
                    &CLI_DETAIL_TRASH
                }
                "mcp" => &CLI_DETAIL_MCP,
                "logs" | "logs clear" => &CLI_DETAIL_LOGS,
                "work" => &CLI_DETAIL_WORK,
                "restore" => &CLI_DETAIL_RESTORE,
                "bench" | "bench hooks" | "bench queries" | "bench create" => &CLI_DETAIL_BENCH,
                "update" | "update check" | "update list" | "update install" | "update use"
                | "update uninstall" | "update where" | "update completions" => &CLI_DETAIL_UPDATE,
                _ => {
                    let err = CliReferenceError {
                        error: format!(
                            "Unknown command: '{}'. Call cli_reference without a command argument to see all available commands.",
                            cmd
                        ),
                    };
                    return Ok(to_string_pretty(&err)?);
                }
            };
            Ok(to_string_pretty(detail)?)
        }
    }
}
