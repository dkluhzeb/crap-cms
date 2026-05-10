//! CLI reference data — 24 CLI_DETAIL_* statics consumed by `exec_cli_reference`.
//!
//! Split out from `cli_reference.rs` to keep that file under the 1000-LOC soft limit.

use super::cli_reference::{CliArg, CliCommandDetail, CliFlag, CliSubcommand};

pub(super) static CLI_DETAIL_SERVE: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_STATUS: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_INIT: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_MAKE: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_BLUEPRINT: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_USER: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_MIGRATE: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_BACKUP: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_DB: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_EXPORT: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_IMPORT: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_TYPEGEN: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_PROTO: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_TEMPLATES: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_JOBS: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_IMAGES: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_TRASH: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_MCP: CliCommandDetail = CliCommandDetail {
    command: "crap-cms mcp",
    description: "Start the MCP (Model Context Protocol) server using stdio transport",
    flags: None,
    args: None,
    subcommands: None,
    examples: Some(&["crap-cms mcp"]),
};

pub(super) static CLI_DETAIL_LOGS: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_WORK: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_RESTORE: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_BENCH: CliCommandDetail = CliCommandDetail {
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

pub(super) static CLI_DETAIL_UPDATE: CliCommandDetail = CliCommandDetail {
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
