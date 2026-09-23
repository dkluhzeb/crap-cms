//! The clap root: the `crap-cms` binary's argument parser.
//!
//! Lives in the library rather than in `main.rs` so anything that needs to
//! introspect the CLI — the `cli_reference` MCP tool, shell completions —
//! can reach the same [`clap::Command`] tree the binary parses with, via
//! [`clap::CommandFactory`]. `main.rs` owns only `Cli::parse()` and the
//! dispatch that follows it.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::commands::{
    BenchAction, BlueprintAction, DbAction, ImagesAction, JobsAction, LogsAction, MakeAction,
    MigrateAction, TemplatesAction, TrashAction, TypegenAction, UpdateCmd, UserAction,
    serve::ServeMode,
};

#[derive(Parser)]
#[command(
    name = "crap-cms",
    about = "Crap CMS - Headless CMS with Lua hooks",
    version
)]
pub struct Cli {
    /// Path to the config directory (auto-detected from CWD if omitted)
    #[arg(short = 'C', long, global = true, env = "CRAP_CONFIG_DIR")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Start the admin UI and gRPC servers
    Serve {
        /// Run in the background (detached)
        #[arg(short, long, conflicts_with_all = ["stop", "restart", "status"])]
        detach: bool,

        /// Stop a running detached instance
        #[arg(long, conflicts_with_all = ["detach", "restart", "status"])]
        stop: bool,

        /// Restart a running detached instance (stop + start)
        #[arg(long, conflicts_with_all = ["detach", "stop", "status"])]
        restart: bool,

        /// Show status of a detached instance
        #[arg(long, conflicts_with_all = ["detach", "stop", "restart"])]
        status: bool,

        /// Output logs as structured JSON (for log aggregation)
        #[arg(long)]
        json: bool,

        /// Start only the specified server (admin or grpc). Omit to start both.
        #[arg(long, value_enum)]
        only: Option<ServeMode>,

        /// Disable the background job scheduler
        #[arg(long)]
        no_scheduler: bool,
    },

    /// Run a standalone job worker (processes queues without HTTP/gRPC servers)
    Work {
        /// Run in the background (detached).
        #[arg(short, long, conflicts_with_all = ["stop", "restart", "status"])]
        detach: bool,

        /// Stop a running detached worker.
        #[arg(long, conflicts_with_all = ["detach", "restart", "status"])]
        stop: bool,

        /// Restart a running detached worker (stop + start).
        #[arg(long, conflicts_with_all = ["detach", "stop", "status"])]
        restart: bool,

        /// Show status of a detached worker.
        #[arg(long, conflicts_with_all = ["detach", "stop", "restart"])]
        status: bool,

        /// Process only specific queues (comma-separated). Default: all queues.
        #[arg(long, value_delimiter = ',')]
        queues: Option<Vec<String>>,

        /// Override max concurrent jobs for this worker.
        #[arg(long)]
        concurrency: Option<usize>,

        /// Skip cron scheduling (let another worker handle it).
        #[arg(long)]
        no_cron: bool,
    },

    /// Show project status (collections, globals, migrations)
    Status {
        /// Run best-practice health checks on configuration and project state
        #[arg(long)]
        check: bool,
    },

    /// User management for auth collections
    #[command(name = "user")]
    User {
        #[command(subcommand)]
        action: UserAction,
    },

    /// Scaffold a new config directory
    Init {
        /// Directory to create (prompted if omitted)
        dir: Option<PathBuf>,

        /// Non-interactive mode — skip all prompts, use defaults
        #[arg(long)]
        no_input: bool,
    },

    /// Generate scaffolding files (collection, global, hook, migration)
    Make {
        #[command(subcommand)]
        action: MakeAction,
    },

    /// Manage saved blueprints
    Blueprint {
        #[command(subcommand)]
        action: BlueprintAction,
    },

    /// Generate typed definitions from collection schemas
    Typegen {
        #[command(subcommand)]
        action: TypegenAction,
    },

    /// Export the embedded content.proto file for gRPC client codegen
    Proto {
        /// Output path (file or directory). Omit to write to stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Run database migrations
    #[command(name = "migrate")]
    Migrate {
        #[command(subcommand)]
        action: MigrateAction,
    },

    /// Backup database and optionally uploads
    Backup {
        /// Output directory (default: <`config_dir>/backups`/)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Also compress the uploads directory
        #[arg(short, long)]
        include_uploads: bool,

        /// Run even if the config fails validation (recovery after an upgrade);
        /// the validation error is printed as a warning
        #[arg(long)]
        skip_config_validation: bool,
    },

    /// Restore database (and optionally uploads) from a backup directory
    Restore {
        /// Path to the backup directory (e.g. backups/backup-2026-03-07T10-00-00)
        backup: PathBuf,

        /// Also restore uploads from uploads.tar.gz if present
        #[arg(short, long)]
        include_uploads: bool,

        /// Confirm destructive operation (required)
        #[arg(short = 'y', long)]
        confirm: bool,

        /// Run even if the config fails validation (recovery after an upgrade);
        /// the validation error is printed as a warning
        #[arg(long)]
        skip_config_validation: bool,
    },

    /// Database tools
    Db {
        #[command(subcommand)]
        action: DbAction,
    },

    /// Export collection data to JSON
    Export {
        /// Export only this collection (default: all)
        #[arg(short, long)]
        collection: Option<String>,

        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Also export each account's password hash, lock, session version,
        /// verification and TOTP state (treat the file like a database dump)
        #[arg(long)]
        include_credentials: bool,
    },

    /// Import collection data from JSON (raw upsert)
    ///
    /// Documents whose `id` already exists are updated, others are
    /// created. Writes are raw: hooks and validators do NOT run.
    /// Reference counts are kept consistent automatically.
    Import {
        /// JSON file to import
        file: PathBuf,

        /// Import only this collection (default: all in file)
        #[arg(short, long)]
        collection: Option<String>,
    },

    /// Manage admin template / static customizations: list, extract, status, diff
    Templates {
        #[command(subcommand)]
        action: TemplatesAction,
    },

    /// Manage background jobs
    Jobs {
        #[command(subcommand)]
        action: JobsAction,
    },

    /// Manage image processing queue
    Images {
        #[command(subcommand)]
        action: ImagesAction,
    },

    /// Manage soft-deleted documents (trash)
    Trash {
        #[command(subcommand)]
        action: TrashAction,
    },

    /// Start the MCP (Model Context Protocol) server (stdio transport)
    Mcp,

    /// View and manage log files
    // `--follow`, `--lines` and `--skip-config-validation` belong to the tail;
    // a subcommand such as `clear` refuses them rather than silently ignoring
    // them (`clear` deletes files, so it never runs on an invalid config).
    #[command(args_conflicts_with_subcommands = true)]
    Logs {
        /// Follow log output in real time
        #[arg(short, long)]
        follow: bool,

        /// Number of lines to show (default: 100)
        #[arg(short = 'n', long, default_value = "100")]
        lines: usize,

        /// Run even if the config fails validation (recovery after an upgrade);
        /// the validation error is printed as a warning
        #[arg(long)]
        skip_config_validation: bool,

        #[command(subcommand)]
        action: Option<LogsAction>,
    },

    /// Benchmark hooks, queries, and write cycles
    Bench {
        #[command(subcommand)]
        action: BenchAction,
    },

    /// Format Handlebars templates (.hbs)
    Fmt {
        /// Paths to format. Files or directories. Defaults to `templates/`.
        paths: Vec<PathBuf>,

        /// Don't write — exit non-zero if any file would change. CI gate.
        #[arg(long)]
        check: bool,

        /// Read source from stdin and write the formatted result to stdout.
        /// Used by editor formatter integrations.
        #[arg(long, conflicts_with = "check")]
        stdio: bool,

        /// Follow symlinks. Off by default: symlinked directories are not
        /// descended and a symlinked `.hbs` is not written through to its
        /// target (which may live outside the tree).
        #[arg(long)]
        follow_symlinks: bool,
    },

    /// Manage installed versions of crap-cms
    Update {
        /// Skip confirmation prompts. Only bare `update` and `update use --force`
        /// prompt; every other subcommand ignores it.
        #[arg(short = 'y', long, global = true)]
        yes: bool,

        /// Allow bare `update` and `update use` even when the binary looks
        /// distro-managed, and repoint the `crap-cms` on `$PATH` at the store.
        #[arg(long, global = true)]
        force: bool,

        #[command(subcommand)]
        action: Option<UpdateCmd>,
    },
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser as _};

    use super::Cli;

    /// Regression: `logs -f clear` / `logs -n 5 clear` parsed and silently
    /// ignored the tail flags.
    #[test]
    fn logs_clear_refuses_the_tail_flags() {
        assert!(Cli::try_parse_from(["crap-cms", "logs", "-f", "clear"]).is_err());
        assert!(Cli::try_parse_from(["crap-cms", "logs", "-n", "5", "clear"]).is_err());
        assert!(
            Cli::try_parse_from(["crap-cms", "logs", "--skip-config-validation", "clear"]).is_err()
        );

        assert!(Cli::try_parse_from(["crap-cms", "logs", "clear"]).is_ok());
        assert!(Cli::try_parse_from(["crap-cms", "logs", "-f", "-n", "5"]).is_ok());
    }

    /// The binary's own contract: the parser builds, every argument is
    /// consistent, and the root keeps the name the docs and completions
    /// are written against.
    #[test]
    fn the_clap_root_builds() {
        let mut cmd = Cli::command();
        cmd.build();

        assert_eq!(cmd.get_name(), "crap-cms");
        assert!(cmd.get_subcommands().any(|s| s.get_name() == "serve"));
    }
}
