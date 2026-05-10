//! `cli_reference` MCP tool — return CLI documentation for the AI client.
//!
//! The 23 `CLI_DETAIL_*` statics (one per top-level command) live in the
//! sibling `cli_details` module to keep this file under the 1000-LOC soft
//! limit. Types defined here are `pub(super)` so the data file can name them.

use anyhow::Result;
use serde::Serialize;
use serde_json::to_string_pretty;

use super::cli_details::{
    CLI_DETAIL_BACKUP, CLI_DETAIL_BENCH, CLI_DETAIL_BLUEPRINT, CLI_DETAIL_DB, CLI_DETAIL_EXPORT,
    CLI_DETAIL_IMAGES, CLI_DETAIL_IMPORT, CLI_DETAIL_INIT, CLI_DETAIL_JOBS, CLI_DETAIL_LOGS,
    CLI_DETAIL_MAKE, CLI_DETAIL_MCP, CLI_DETAIL_MIGRATE, CLI_DETAIL_PROTO, CLI_DETAIL_RESTORE,
    CLI_DETAIL_SERVE, CLI_DETAIL_STATUS, CLI_DETAIL_TEMPLATES, CLI_DETAIL_TRASH,
    CLI_DETAIL_TYPEGEN, CLI_DETAIL_UPDATE, CLI_DETAIL_USER, CLI_DETAIL_WORK,
};

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
pub(super) struct CliCommandDetail {
    pub command: &'static str,
    pub description: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<&'static [CliFlag]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<&'static [CliArg]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subcommands: Option<&'static [CliSubcommand]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples: Option<&'static [&'static str]>,
}

#[derive(Serialize)]
pub(super) struct CliFlag {
    pub flag: &'static str,
    pub description: &'static str,
}

#[derive(Serialize)]
pub(super) struct CliArg {
    pub arg: &'static str,
    pub description: &'static str,
}

#[derive(Serialize)]
pub(super) struct CliSubcommand {
    pub name: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<&'static [CliFlag]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples: Option<&'static [&'static str]>,
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

#[cfg(test)]
mod tests {
    use serde_json::{Value, from_str};

    use super::*;

    #[test]
    fn cli_reference_all_commands() {
        let result = exec_cli_reference(None).unwrap();
        let parsed: Value = from_str(&result).unwrap();
        let commands = parsed["commands"].as_array().unwrap();
        assert!(commands.len() >= 15);

        let names: Vec<&str> = commands
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        for expected in &["serve", "migrate", "user", "backup", "jobs", "mcp"] {
            assert!(names.contains(expected), "Missing command: {}", expected);
        }
    }

    #[test]
    fn cli_reference_specific_command() {
        let result = exec_cli_reference(Some("migrate")).unwrap();
        let parsed: Value = from_str(&result).unwrap();
        assert!(parsed.get("subcommands").is_some());
        let subs = parsed["subcommands"].as_array().unwrap();
        let sub_names: Vec<&str> = subs.iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert!(sub_names.contains(&"up"));
        assert!(sub_names.contains(&"down"));
        assert!(sub_names.contains(&"create"));
        assert!(sub_names.contains(&"list"));
        assert!(sub_names.contains(&"fresh"));
    }

    #[test]
    fn cli_reference_unknown_command() {
        let result = exec_cli_reference(Some("nonexistent")).unwrap();
        let parsed: Value = from_str(&result).unwrap();
        assert!(parsed.get("error").is_some());
    }
}
