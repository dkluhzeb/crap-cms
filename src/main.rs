//! CLI entrypoint for Crap CMS. Parses flags, loads config, and starts the admin + gRPC servers.
//!
//! Subcommands: `serve`, `status`, `user`, `make`, `blueprint`, `db`, `typegen`, `proto`,
//! `migrate`, `backup`, `export`, `import`, `init`, `templates`, `jobs`, `images`, `trash`,
//! `logs`, `mcp`.
//! Running bare `crap-cms` prints help.

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use dialoguer::Select;
use std::path::{Path, PathBuf};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    Layer, Registry, fmt::writer::BoxMakeWriter, layer::SubscriberExt, util::SubscriberInitExt,
};

use crap_cms::{
    cli::{self, crap_theme},
    commands::{
        self, BenchAction, BlueprintAction, DbAction, ImagesAction, JobsAction, LogsAction,
        MakeAction, MigrateAction, TemplatesAction, TrashAction, TypegenAction, UpdateCmd,
        UserAction, serve::ServeMode,
    },
    config::{CrapConfig, LogRotation},
};

#[derive(Parser)]
#[command(
    name = "crap-cms",
    about = "Crap CMS - Headless CMS with Lua hooks",
    version
)]
struct Cli {
    /// Path to the config directory (auto-detected from CWD if omitted)
    #[arg(short = 'C', long, global = true, env = "CRAP_CONFIG_DIR")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
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
    Logs {
        /// Follow log output in real time
        #[arg(short, long)]
        follow: bool,

        /// Number of lines to show (default: 100)
        #[arg(short = 'n', long, default_value = "100")]
        lines: usize,

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
        /// Skip confirmation prompts (no-op for read-only subcommands).
        #[arg(short = 'y', long, global = true)]
        yes: bool,

        /// Allow self-update even when the binary looks distro-managed.
        #[arg(long, global = true)]
        force: bool,

        #[command(subcommand)]
        action: Option<UpdateCmd>,
    },
}

/// Binary entrypoint — parses CLI args and dispatches to the appropriate command.
#[cfg(not(tarpaulin_include))]
#[tokio::main]
async fn main() {
    let cli_args = Cli::parse();

    // Box the future: `run` covers every CLI command's startup path, so the
    // generated state machine is ~18 KiB. Heap-allocating avoids blowing the
    // top-level task's stack on debug builds.
    if let Err(e) = Box::pin(run(cli_args)).await {
        cli::error(&format!("{e:#}"));
        std::process::exit(1);
    }
}

/// Args for the `serve` command dispatcher — grouped because the
/// underlying Clap variant holds the same set of fields and they're all
/// required.
struct ServeArgs {
    config_flag: Option<PathBuf>,
    detach: bool,
    stop: bool,
    restart: bool,
    status: bool,
    json: bool,
    only: Option<ServeMode>,
    no_scheduler: bool,
}

/// Args for the `work` command dispatcher.
struct WorkArgs {
    config_flag: Option<PathBuf>,
    detach: bool,
    stop: bool,
    restart: bool,
    status: bool,
    queues: Option<Vec<String>>,
    concurrency: Option<usize>,
    no_cron: bool,
}

/// Dispatch `serve` (and its `--stop/--status/--restart/--detach` variants).
///
/// Accepts the full [`Command`] enum (rather than a pre-unpacked
/// `ServeArgs`) so the caller in [`dispatch_command`] stays a single
/// line per arm. Panics if the variant isn't `Command::Serve`; routing
/// is the caller's responsibility.
#[cfg(not(tarpaulin_include))]
async fn dispatch_serve(command: Command, config_flag: Option<PathBuf>) -> Result<()> {
    let Command::Serve {
        detach,
        stop,
        restart,
        status,
        json,
        only,
        no_scheduler,
    } = command
    else {
        unreachable!("dispatch_serve called with non-Serve command");
    };
    let args = ServeArgs {
        config_flag,
        detach,
        stop,
        restart,
        status,
        json,
        only,
        no_scheduler,
    };
    let config = commands::resolve_config_dir(args.config_flag)?;
    if args.stop {
        #[cfg(unix)]
        return commands::serve::stop(&config);
        #[cfg(not(unix))]
        anyhow::bail!("--stop is not supported on this platform");
    }
    if args.status {
        #[cfg(unix)]
        return commands::serve::status(&config);
        #[cfg(not(unix))]
        anyhow::bail!("--status is not supported on this platform");
    }
    if args.restart {
        #[cfg(unix)]
        return commands::serve::restart(&config, args.only, args.no_scheduler, args.json);
        #[cfg(not(unix))]
        anyhow::bail!("--restart is not supported on this platform");
    }
    if args.detach {
        return commands::serve::detach(&config, args.only, args.no_scheduler, args.json);
    }
    // Box the future: serve startup wires both admin+API servers and
    // is the largest async state machine in the binary.
    Box::pin(commands::serve::run(&config, args.only, args.no_scheduler)).await
}

/// Dispatch `work` (and its `--stop/--status/--restart/--detach` variants).
///
/// See [`dispatch_serve`] for the rationale behind taking the full
/// [`Command`] enum instead of a pre-unpacked `WorkArgs`. Panics if the
/// variant isn't `Command::Work`; routing is the caller's responsibility.
#[cfg(not(tarpaulin_include))]
async fn dispatch_work(command: Command, config_flag: Option<PathBuf>) -> Result<()> {
    let Command::Work {
        detach,
        stop,
        restart,
        status,
        queues,
        concurrency,
        no_cron,
    } = command
    else {
        unreachable!("dispatch_work called with non-Work command");
    };
    let args = WorkArgs {
        config_flag,
        detach,
        stop,
        restart,
        status,
        queues,
        concurrency,
        no_cron,
    };
    let config = commands::resolve_config_dir(args.config_flag)?;
    if args.stop {
        #[cfg(unix)]
        return commands::work::stop(&config);
        #[cfg(not(unix))]
        anyhow::bail!("--stop is not supported on this platform");
    }
    if args.status {
        #[cfg(unix)]
        return commands::work::status(&config);
        #[cfg(not(unix))]
        anyhow::bail!("--status is not supported on this platform");
    }
    if args.restart {
        #[cfg(unix)]
        return commands::work::restart(
            &config,
            args.queues.as_deref(),
            args.concurrency,
            args.no_cron,
        );
        #[cfg(not(unix))]
        anyhow::bail!("--restart is not supported on this platform");
    }
    if args.detach {
        return commands::work::detach(
            &config,
            args.queues.as_deref(),
            args.concurrency,
            args.no_cron,
        );
    }
    commands::work::run(&config, args.queues, args.concurrency, args.no_cron).await
}

/// Dispatch `blueprint` subcommands. The interactive `Select` prompts
/// for `Use` / `Remove` when no name is supplied stay in this helper —
/// they're command-specific UI rather than shared CLI plumbing.
#[cfg(not(tarpaulin_include))]
fn dispatch_blueprint(action: BlueprintAction, config_flag: Option<PathBuf>) -> Result<()> {
    match action {
        BlueprintAction::Save { name, force } => {
            let config = commands::resolve_config_dir(config_flag)?;
            crap_cms::scaffold::blueprint_save(&config, &name, force)
        }
        BlueprintAction::Use { name, dir } => {
            let name = if let Some(n) = name {
                n
            } else {
                let names = crap_cms::scaffold::list_blueprint_names()?;

                if names.is_empty() {
                    bail!(
                        "No blueprints saved yet.\nSave one with: crap-cms blueprint save <name>"
                    );
                }
                let selection = Select::with_theme(&crap_theme())
                    .with_prompt("Select blueprint")
                    .items(&names)
                    .interact()
                    .context("Failed to read blueprint selection")?;
                names[selection].clone()
            };
            crap_cms::scaffold::blueprint_use(&name, dir)
        }
        BlueprintAction::List => crap_cms::scaffold::blueprint_list(),
        BlueprintAction::Remove { name } => {
            let name = if let Some(n) = name {
                n
            } else {
                let names = crap_cms::scaffold::list_blueprint_names()?;

                if names.is_empty() {
                    bail!("No blueprints saved yet.");
                }
                let selection = Select::with_theme(&crap_theme())
                    .with_prompt("Select blueprint to remove")
                    .items(&names)
                    .interact()
                    .context("Failed to read blueprint selection")?;
                names[selection].clone()
            };
            crap_cms::scaffold::blueprint_remove(&name)
        }
    }
}

/// Dispatch `templates` subcommands (list / extract / status / diff / layout).
#[cfg(not(tarpaulin_include))]
fn dispatch_templates(action: TemplatesAction, config_flag: Option<PathBuf>) -> Result<()> {
    match action {
        TemplatesAction::List { r#type, verbose } => {
            commands::templates::list(r#type.as_deref(), verbose)
        }
        TemplatesAction::Extract {
            paths,
            all,
            r#type,
            force,
        } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::templates::extract(&config, &paths, all, r#type.as_deref(), force)
        }
        TemplatesAction::Status => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::templates::status(&config)
        }
        TemplatesAction::Diff { path } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::templates::diff(&config, &path)
        }
        TemplatesAction::Layout => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::templates::layout(&config)
        }
    }
}

/// Pre-match logging setup result: optional file-logging config (for
/// long-running commands) plus the resolved `dev_mode` flag used to pick
/// the default tracing filter.
struct LoggingSetup {
    serve_logging: Option<(PathBuf, crap_cms::config::LoggingConfig)>,
    dev_mode: bool,
}

/// For long-running commands (`serve`, `work`, `mcp`), load config up
/// front so file-logging can be initialized before any tracing call.
/// Auto-enables file logging when the process is a detached child
/// (stdout/stderr go to /dev/null in that case).
#[cfg(not(tarpaulin_include))]
fn prepare_logging_setup(
    command: &Command,
    config_flag: Option<PathBuf>,
    is_detached_child: bool,
) -> Result<LoggingSetup> {
    let is_long_running = matches!(
        command,
        Command::Serve { .. } | Command::Work { .. } | Command::Mcp
    );
    if !is_long_running {
        return Ok(LoggingSetup {
            serve_logging: None,
            dev_mode: false,
        });
    }

    let config_dir = commands::resolve_config_dir(config_flag)?;
    let mut config = CrapConfig::load(&config_dir)?;

    if is_detached_child && !config.logging.file {
        config.logging.file = true;
    }

    Ok(LoggingSetup {
        serve_logging: Some((config_dir, config.logging)),
        dev_mode: config.admin.dev_mode,
    })
}

/// Parse the CLI, set up logging, and hand off to [`dispatch_command`].
#[cfg(not(tarpaulin_include))]
async fn run(cli: Cli) -> Result<()> {
    let use_json = matches!(&cli.command, Command::Serve { json: true, .. })
        || std::env::var("CRAP_LOG_FORMAT").is_ok_and(|v| v == "json");

    // _CRAP_DETACHED is set by detach() on the child process.
    let is_detached_child = std::env::var("_CRAP_DETACHED").is_ok();

    let config_flag = cli.config;

    let logging_setup =
        prepare_logging_setup(&cli.command, config_flag.clone(), is_detached_child)?;

    let default_filter = match &cli.command {
        Command::Serve { .. } | Command::Work { .. } if logging_setup.dev_mode => {
            "crap_cms=debug,info"
        }
        Command::Serve { .. } | Command::Work { .. } | Command::Mcp => "crap_cms=info",
        _ => "crap_cms=error",
    };

    let _guard = init_logging(
        use_json,
        default_filter,
        logging_setup.serve_logging.as_ref(),
        console_logs_to_stderr(&cli.command),
    );

    dispatch_command(cli.command, config_flag).await
}

/// Whether the console log layer must write to stderr instead of the default
/// stdout. True for `mcp`: its stdio JSON-RPC transport owns stdout
/// (`src/mcp/stdio.rs`), so any log line on stdout corrupts the protocol
/// stream. Everything else keeps logging to stdout (the frozen default).
fn console_logs_to_stderr(command: &Command) -> bool {
    matches!(command, Command::Mcp)
}

/// Resolve the config directory from the optional `-C/--config` flag and
/// hand the resolved path to `f`. Used to compress the dozens of CLI
/// dispatch arms that follow the same `let c = resolve_config_dir(...)?;
/// commands::X::run(&c, ...)` pattern into single-line expressions.
#[cfg(not(tarpaulin_include))]
fn with_config<F, R>(config_flag: Option<PathBuf>, f: F) -> Result<R>
where
    F: FnOnce(&Path) -> Result<R>,
{
    let config = commands::resolve_config_dir(config_flag)?;
    f(&config)
}

/// Big-match dispatcher mapping each parsed `Command` variant to the
/// command-specific handler module. Verbose variants (`serve`, `work`,
/// `blueprint`, `templates`) delegate to dedicated dispatchers; smaller
/// variants use `with_config` to keep each arm to a single expression.
#[cfg(not(tarpaulin_include))]
async fn dispatch_command(command: Command, config_flag: Option<PathBuf>) -> Result<()> {
    match command {
        cmd @ Command::Serve { .. } => dispatch_serve(cmd, config_flag).await,
        cmd @ Command::Work { .. } => dispatch_work(cmd, config_flag).await,
        Command::Status { check } => with_config(config_flag, |c| commands::status::run(c, check)),
        Command::User { action } => with_config(config_flag, |c| commands::user::run(c, action)),
        Command::Init { dir, no_input } => commands::init::run(dir, no_input),
        Command::Make { action } => with_config(config_flag, |c| commands::make::run(c, action)),
        Command::Blueprint { action } => dispatch_blueprint(action, config_flag),
        Command::Typegen { action } => {
            with_config(config_flag, |c| commands::typegen::run(c, action))
        }
        Command::Proto { output } => crap_cms::scaffold::proto_export(output.as_deref()),
        Command::Migrate { action } => {
            with_config(config_flag, |c| commands::db::migrate(c, &action))
        }
        Command::Backup {
            output,
            include_uploads,
        } => with_config(config_flag, |c| {
            commands::db::backup(c, output, include_uploads)
        }),
        Command::Restore {
            backup,
            include_uploads,
            confirm,
        } => with_config(config_flag, |c| {
            commands::db::restore(c, &backup, include_uploads, confirm)
        }),
        Command::Db { action } => with_config(config_flag, |c| match action {
            DbAction::Console => commands::db::console(c),
            DbAction::Cleanup { confirm } => commands::db::cleanup(c, confirm),
        }),
        Command::Export { collection, output } => with_config(config_flag, |c| {
            commands::export::export(c, collection.as_deref(), output)
        }),
        Command::Import { file, collection } => with_config(config_flag, |c| {
            commands::export::import(c, &file, collection.as_deref())
        }),
        Command::Templates { action } => dispatch_templates(action, config_flag),
        Command::Jobs { action } => with_config(config_flag, |c| commands::jobs::run(c, action)),
        Command::Images { action } => {
            with_config(config_flag, |c| commands::images::run(c, action))
        }
        Command::Trash { action } => with_config(config_flag, |c| commands::trash::run(action, c)),
        Command::Mcp => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::mcp::run(&config).await
        }
        Command::Logs {
            follow,
            lines,
            action,
        } => with_config(config_flag, |c| {
            commands::logs::run(c, action, follow, lines)
        }),
        Command::Bench { action } => with_config(config_flag, |c| commands::bench::run(c, action)),
        Command::Fmt {
            paths,
            check,
            stdio,
            follow_symlinks,
        } => commands::fmt::run(paths, check, stdio, follow_symlinks),
        Command::Update { yes, force, action } => {
            // Run on a blocking thread — `reqwest::blocking` spawns its own
            // tokio runtime internally, and dropping that while inside
            // `#[tokio::main]` panics. spawn_blocking isolates it.
            tokio::task::spawn_blocking(move || commands::update::run::<Cli>(action, yes, force))
                .await
                .context("update task panicked")?
        }
    }
}

/// Initialize the tracing subscriber with stdout and optional file logging.
///
/// Returns an optional `WorkerGuard` that must be kept alive for the process
/// lifetime to ensure all buffered log entries are flushed to the file.
#[cfg(not(tarpaulin_include))]
fn init_logging(
    use_json: bool,
    default_filter: &str,
    serve_logging: Option<&(PathBuf, crap_cms::config::LoggingConfig)>,
    console_to_stderr: bool,
) -> Option<WorkerGuard> {
    type BoxedLayer = Box<dyn Layer<Registry> + Send + Sync>;

    let mut guard = None;
    let mut layers: Vec<BoxedLayer> = Vec::new();

    // Console layer — stdout by default, stderr when the command owns stdout
    // for something else (e.g. mcp's stdio JSON-RPC transport).
    let console_writer = if console_to_stderr {
        BoxMakeWriter::new(std::io::stderr)
    } else {
        BoxMakeWriter::new(std::io::stdout)
    };

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));

    if use_json {
        layers.push(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(console_writer)
                .with_filter(env_filter)
                .boxed(),
        );
    } else {
        layers.push(
            tracing_subscriber::fmt::layer()
                .with_writer(console_writer)
                .with_filter(env_filter)
                .boxed(),
        );
    }

    // File layer (only when file logging is enabled for serve).
    if let Some((config_dir, logging)) = serve_logging
        && logging.file
        && let Some(file_layer) = build_file_layer(config_dir, logging, use_json, &mut guard)
    {
        layers.push(file_layer);
    }

    tracing_subscriber::registry().with(layers).init();
    guard
}

/// Build the file logging layer with rotation and non-blocking writes.
#[cfg(not(tarpaulin_include))]
fn build_file_layer(
    config_dir: &Path,
    logging: &crap_cms::config::LoggingConfig,
    use_json: bool,
    guard: &mut Option<WorkerGuard>,
) -> Option<Box<dyn Layer<tracing_subscriber::Registry> + Send + Sync>> {
    let p = Path::new(&logging.path);
    let log_dir = if p.is_absolute() {
        p.to_path_buf()
    } else {
        config_dir.join(p)
    };

    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        eprintln!(
            "Failed to create log directory {}: {}",
            log_dir.display(),
            e
        );
        return None;
    }

    let appender = match logging.rotation {
        LogRotation::Hourly => tracing_appender::rolling::hourly(&log_dir, "crap-cms.log"),
        LogRotation::Daily => tracing_appender::rolling::daily(&log_dir, "crap-cms.log"),
        LogRotation::Never => tracing_appender::rolling::never(&log_dir, "crap-cms.log"),
    };

    let (non_blocking, file_guard) = tracing_appender::non_blocking(appender);
    *guard = Some(file_guard);

    let file_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("crap_cms=debug,info"));

    if use_json {
        Some(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(non_blocking)
                .with_filter(file_filter)
                .boxed(),
        )
    } else {
        Some(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(non_blocking)
                .with_filter(file_filter)
                .boxed(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_mcp_routes_console_logs_to_stderr() {
        // mcp owns stdout for the JSON-RPC transport → logs must go to stderr.
        assert!(console_logs_to_stderr(&Command::Mcp));
        // Every other command keeps the frozen stdout default.
        assert!(!console_logs_to_stderr(&Command::Status { check: false }));
        assert!(!console_logs_to_stderr(&Command::Proto { output: None }));
    }
}
