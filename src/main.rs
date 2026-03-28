//! CLI entrypoint for Crap CMS. Parses flags, loads config, and starts the admin + gRPC servers.
//!
//! Subcommands: `serve`, `status`, `user`, `make`, `blueprint`, `db`, `typegen`, `proto`,
//! `migrate`, `backup`, `export`, `import`, `init`, `templates`, `jobs`, `images`, `trash`.
//! Running bare `crap-cms` prints help.

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use dialoguer::Select;
use std::path::PathBuf;

use crap_cms::{
    cli::{self, crap_theme},
    commands::{
        self, BlueprintAction, DbAction, ImagesAction, JobsAction, MakeAction, MigrateAction,
        TemplatesAction, TrashAction, UserAction, serve::ServeMode,
    },
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
    Status,

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

    /// Manage soft-deleted documents (trash)
    Trash {
        #[command(subcommand)]
        action: TrashAction,
    },

    /// Start the MCP (Model Context Protocol) server (stdio transport)
    Mcp,
}

#[cfg(not(tarpaulin_include))] // binary entrypoint — not unit-testable
#[tokio::main]
async fn main() {
    let cli_args = Cli::parse();

    if let Err(e) = run(cli_args).await {
        cli::error(&format!("{:#}", e));
        std::process::exit(1);
    }
}

#[cfg(not(tarpaulin_include))]
async fn run(cli: Cli) -> Result<()> {
    // Initialize tracing subscriber early so all commands get logging.
    // RUST_LOG env overrides. Default: crap_cms=debug for serve, info for others.
    let use_json = matches!(&cli.command, Command::Serve { json: true, .. })
        || std::env::var("CRAP_LOG_FORMAT")
            .map(|v| v == "json")
            .unwrap_or(false);

    let default_filter = match &cli.command {
        Command::Serve { .. } => "crap_cms=debug,info",
        _ => "crap_cms=error",
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

    let config_flag = cli.config;

    match cli.command {
        Command::Serve {
            detach,
            only,
            no_scheduler,
            ..
        } => {
            let config = commands::resolve_config_dir(config_flag)?;
            if detach {
                return commands::serve::detach(&config, only, no_scheduler);
            }
            commands::serve::run(&config, only, no_scheduler).await
        }
        Command::Status => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::status::run(&config)
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
        Command::Typegen { lang, output } => {
            let config = commands::resolve_config_dir(config_flag)?;
            commands::typegen::run(&config, &lang, output.as_deref())
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
    }
}
