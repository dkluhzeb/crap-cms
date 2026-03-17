//! CLI entrypoint for Crap CMS. Parses flags, loads config, and starts the admin + gRPC servers.
//!
//! Subcommands: `serve`, `status`, `user`, `make`, `blueprint`, `db`, `typegen`, `proto`,
//! `migrate`, `backup`, `export`, `import`, `init`, `templates`, `jobs`, `images`.
//! Running bare `crap-cms` prints help.

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crap_cms::commands::{
    self, BlueprintAction, DbAction, ImagesAction, JobsAction, MakeAction, MigrateAction,
    TemplatesAction, UserAction, serve::ServeMode,
};

#[derive(Parser)]
#[command(
    name = "crap-cms",
    about = "Crap CMS - Headless CMS with Lua hooks",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the admin UI and gRPC servers
    Serve {
        /// Path to the config directory
        config: PathBuf,

        /// Run in the background (detached)
        #[arg(short, long)]
        detach: bool,

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

    /// Show project status (collections, globals, migrations)
    Status {
        /// Path to the config directory
        config: PathBuf,
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
        /// Path to the config directory
        config: PathBuf,

        /// Output language: lua, ts, go, py, rs (default: lua). Use "all" for all languages.
        #[arg(short, long, default_value = "lua")]
        lang: String,

        /// Output directory for generated files (default: <config>/types/)
        #[arg(short, long)]
        output: Option<PathBuf>,
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
        /// Path to the config directory
        config: PathBuf,

        #[command(subcommand)]
        action: MigrateAction,
    },

    /// Backup database and optionally uploads
    Backup {
        /// Path to the config directory
        config: PathBuf,

        /// Output directory (default: <config_dir>/backups/)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Also compress the uploads directory
        #[arg(short, long)]
        include_uploads: bool,
    },

    /// Restore database (and optionally uploads) from a backup directory
    Restore {
        /// Path to the config directory
        config: PathBuf,

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
        /// Path to the config directory
        config: PathBuf,

        /// Export only this collection (default: all)
        #[arg(short, long)]
        collection: Option<String>,

        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    /// Import collection data from JSON
    Import {
        /// Path to the config directory
        config: PathBuf,

        /// JSON file to import
        file: PathBuf,

        /// Import only this collection (default: all in file)
        #[arg(short, long)]
        collection: Option<String>,
    },

    /// List and extract default admin templates and static files
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

    /// Start the MCP (Model Context Protocol) server (stdio transport)
    Mcp {
        /// Path to the config directory
        config: PathBuf,
    },
}

#[cfg(not(tarpaulin_include))] // binary entrypoint — not unit-testable
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize tracing subscriber early so all commands get logging.
    // RUST_LOG env overrides. Default: crap_cms=debug for serve, info for others.
    let use_json = matches!(&cli.command, Command::Serve { json: true, .. })
        || std::env::var("CRAP_LOG_FORMAT")
            .map(|v| v == "json")
            .unwrap_or(false);

    let default_filter = match &cli.command {
        Command::Serve { .. } => "crap_cms=debug,info",
        _ => "crap_cms=info,warn",
    };
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));

    if use_json {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(env_filter)
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    }

    match cli.command {
        Command::Serve {
            config,
            detach,
            only,
            no_scheduler,
            ..
        } => {
            if detach {
                return commands::serve::detach(&config, only, no_scheduler);
            }
            commands::serve::run(&config, only, no_scheduler).await
        }
        Command::Status { config } => commands::status::run(&config),
        Command::User { action } => commands::user::run(action),
        Command::Init { dir, no_input } => commands::init::run(dir, no_input),
        Command::Make { action } => commands::make::run(action),
        Command::Blueprint { action } => match action {
            BlueprintAction::Save {
                config,
                name,
                force,
            } => crap_cms::scaffold::blueprint_save(&config, &name, force),
            BlueprintAction::Use { name, dir } => {
                let name = match name {
                    Some(n) => n,
                    None => {
                        use dialoguer::Select;
                        let names = crap_cms::scaffold::list_blueprint_names()?;

                        if names.is_empty() {
                            bail!(
                                "No blueprints saved yet.\nSave one with: crap-cms blueprint save <dir> <name>"
                            );
                        }
                        let selection = Select::new()
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
                        use dialoguer::Select;
                        let names = crap_cms::scaffold::list_blueprint_names()?;

                        if names.is_empty() {
                            bail!("No blueprints saved yet.");
                        }
                        let selection = Select::new()
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
            config,
            lang,
            output,
        } => commands::typegen::run(&config, &lang, output.as_deref()),
        Command::Proto { output } => crap_cms::scaffold::proto_export(output.as_deref()),
        Command::Migrate { config, action } => commands::db::migrate(&config, action),
        Command::Backup {
            config,
            output,
            include_uploads,
        } => commands::db::backup(&config, output, include_uploads),
        Command::Restore {
            config,
            backup,
            include_uploads,
            confirm,
        } => commands::db::restore(&config, &backup, include_uploads, confirm),
        Command::Db { action } => match action {
            DbAction::Console { config } => commands::db::console(&config),
            DbAction::Cleanup { config, confirm } => commands::db::cleanup(&config, confirm),
        },
        Command::Export {
            config,
            collection,
            output,
        } => commands::export::export(&config, collection, output),
        Command::Import {
            config,
            file,
            collection,
        } => commands::export::import(&config, &file, collection),
        Command::Templates { action } => commands::templates::run(action),
        Command::Jobs { action } => commands::jobs::run(action),
        Command::Images { action } => commands::images::run(action),
        Command::Mcp { config } => commands::mcp::run(&config).await,
    }
}
