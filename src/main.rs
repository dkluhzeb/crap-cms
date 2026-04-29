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
use tracing_subscriber::{Layer, Registry, layer::SubscriberExt, util::SubscriberInitExt};

use crap_cms::{
    cli::{self, crap_theme},
    commands::{
        self, BenchAction, BlueprintAction, DbAction, ImagesAction, JobsAction, LogsAction,
        MakeAction, MigrateAction, TemplatesAction, TrashAction, UpdateCmd, UserAction,
        serve::ServeMode,
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

        /// Start only the specified server (admin or api). Omit to start both.
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
        /// Output language: lua, ts, go, py, rs (default: lua). Use "all" for all languages.
        #[arg(short, long, default_value = "lua")]
        lang: String,

        /// Output directory for generated files (default: <config>/types/)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Generate prost_types conversion code for Rust. Value is the proto module path
        /// (e.g. "crate::proto"). Writes generated_proto.rs alongside generated.rs.
        #[arg(long)]
        proto: Option<String>,
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
        /// Output directory (default: <config_dir>/backups/)
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

    /// Import collection data from JSON
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

    if let Err(e) = run(cli_args).await {
        cli::error(&format!("{:#}", e));
        std::process::exit(1);
    }
}

/// Dispatch the parsed CLI command to the appropriate handler.
#[cfg(not(tarpaulin_include))]
async fn run(cli: Cli) -> Result<()> {
    let use_json = matches!(&cli.command, Command::Serve { json: true, .. })
        || std::env::var("CRAP_LOG_FORMAT")
            .map(|v| v == "json")
            .unwrap_or(false);

    let is_long_running = matches!(
        &cli.command,
        Command::Serve { .. } | Command::Work { .. } | Command::Mcp
    );

    // _CRAP_DETACHED is set by detach() on the child process.
    let is_detached_child = std::env::var("_CRAP_DETACHED").is_ok();

    let config_flag = cli.config;

    // For serve/work: load config before tracing init so we can set up file logging.
    // Config will be loaded again inside serve::run()/work::run() — intentional and cheap.
    let (serve_logging, dev_mode) = if is_long_running {
        let config_dir = commands::resolve_config_dir(config_flag.clone())?;
        let mut config = CrapConfig::load(&config_dir)?;

        // Auto-enable file logging for detached mode — stdout/stderr go to /dev/null.
        if is_detached_child && !config.logging.file {
            config.logging.file = true;
        }

        let dev = config.admin.dev_mode;
        (Some((config_dir, config.logging)), dev)
    } else {
        (None, false)
    };

    let default_filter = match &cli.command {
        Command::Serve { .. } if dev_mode => "crap_cms=debug,info",
        Command::Serve { .. } => "crap_cms=info",
        Command::Work { .. } if dev_mode => "crap_cms=debug,info",
        Command::Work { .. } => "crap_cms=info",
        Command::Mcp => "crap_cms=info",
        _ => "crap_cms=error",
    };

    let _guard = init_logging(use_json, default_filter, serve_logging.as_ref());

    match cli.command {
        Command::Serve {
            detach,
            stop,
            restart,
            status,
            only,
            no_scheduler,
            ..
        } => {
            let config = commands::resolve_config_dir(config_flag)?;
            if stop {
                #[cfg(unix)]
                return commands::serve::stop(&config);
                #[cfg(not(unix))]
                anyhow::bail!("--stop is not supported on this platform");
            }
            if status {
                #[cfg(unix)]
                return commands::serve::status(&config);
                #[cfg(not(unix))]
                anyhow::bail!("--status is not supported on this platform");
            }
            if restart {
                #[cfg(unix)]
                return commands::serve::restart(&config, only, no_scheduler);
                #[cfg(not(unix))]
                anyhow::bail!("--restart is not supported on this platform");
            }
            if detach {
                return commands::serve::detach(&config, only, no_scheduler);
            }
            commands::serve::run(&config, only, no_scheduler).await
        }
        Command::Work {
            detach,
            stop,
            restart,
            status,
            queues,
            concurrency,
            no_cron,
        } => {
            let config = commands::resolve_config_dir(config_flag)?;
            if stop {
                #[cfg(unix)]
                return commands::work::stop(&config);
                #[cfg(not(unix))]
                anyhow::bail!("--stop is not supported on this platform");
            }
            if status {
                #[cfg(unix)]
                return commands::work::status(&config);
                #[cfg(not(unix))]
                anyhow::bail!("--status is not supported on this platform");
            }
            if restart {
                #[cfg(unix)]
                return commands::work::restart(&config, queues, concurrency, no_cron);
                #[cfg(not(unix))]
                anyhow::bail!("--restart is not supported on this platform");
            }
            if detach {
                return commands::work::detach(&config, queues, concurrency, no_cron);
            }
            commands::work::run(&config, queues, concurrency, no_cron).await
        }
        Command::Status { check } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::status::run(&config, check)
        }
        Command::User { action } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::user::run(&config, action)
        }
        Command::Init { dir, no_input } => commands::init::run(dir, no_input),
        Command::Make { action } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::make::run(&config, action)
        }
        Command::Blueprint { action } => match action {
            BlueprintAction::Save { name, force } => {
                let config = commands::resolve_config_dir(config_flag)?;
                crap_cms::scaffold::blueprint_save(&config, &name, force)
            }
            BlueprintAction::Use { name, dir } => {
                let name = match name {
                    Some(n) => n,
                    None => {
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
                    }
                };
                crap_cms::scaffold::blueprint_use(&name, dir)
            }
            BlueprintAction::List => crap_cms::scaffold::blueprint_list(),
            BlueprintAction::Remove { name } => {
                let name = match name {
                    Some(n) => n,
                    None => {
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
                    }
                };
                crap_cms::scaffold::blueprint_remove(&name)
            }
        },
        Command::Typegen {
            lang,
            output,
            proto,
        } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::typegen::run(&config, &lang, output.as_deref(), proto.as_deref())
        }
        Command::Proto { output } => crap_cms::scaffold::proto_export(output.as_deref()),
        Command::Migrate { action } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::db::migrate(&config, action)
        }
        Command::Backup {
            output,
            include_uploads,
        } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::db::backup(&config, output, include_uploads)
        }
        Command::Restore {
            backup,
            include_uploads,
            confirm,
        } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::db::restore(&config, &backup, include_uploads, confirm)
        }
        Command::Db { action } => {
            let config = commands::resolve_config_dir(config_flag)?;
            match action {
                DbAction::Console => commands::db::console(&config),
                DbAction::Cleanup { confirm } => commands::db::cleanup(&config, confirm),
            }
        }
        Command::Export { collection, output } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::export::export(&config, collection, output)
        }
        Command::Import { file, collection } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::export::import(&config, &file, collection)
        }
        Command::Templates { action } => match action {
            TemplatesAction::List { r#type, verbose } => commands::templates::list(r#type, verbose),
            TemplatesAction::Extract {
                paths,
                all,
                r#type,
                force,
            } => {
                let config = commands::resolve_config_dir(config_flag)?;
                commands::templates::extract(&config, &paths, all, r#type, force)
            }
            TemplatesAction::Status => {
                let config = commands::resolve_config_dir(config_flag)?;
                commands::templates::status(&config)
            }
            TemplatesAction::Diff { path } => {
                let config = commands::resolve_config_dir(config_flag)?;
                commands::templates::diff(&config, &path)
            }
        },
        Command::Jobs { action } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::jobs::run(&config, action)
        }
        Command::Images { action } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::images::run(&config, action)
        }
        Command::Trash { action } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::trash::run(action, &config)
        }
        Command::Mcp => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::mcp::run(&config).await
        }
        Command::Logs {
            follow,
            lines,
            action,
        } => {
            let config_dir = commands::resolve_config_dir(config_flag)?;
            commands::logs::run(&config_dir, action, follow, lines)
        }
        Command::Bench { action } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::bench::run(&config, action)
        }
        Command::Fmt {
            paths,
            check,
            stdio,
        } => commands::fmt::run(paths, check, stdio),
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
) -> Option<WorkerGuard> {
    type BoxedLayer = Box<dyn Layer<Registry> + Send + Sync>;

    let mut guard = None;
    let mut layers: Vec<BoxedLayer> = Vec::new();

    // Stdout layer.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));

    if use_json {
        layers.push(
            tracing_subscriber::fmt::layer()
                .json()
                .with_filter(env_filter)
                .boxed(),
        );
    } else {
        layers.push(
            tracing_subscriber::fmt::layer()
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
