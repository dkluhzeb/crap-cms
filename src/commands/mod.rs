//! CLI command handlers. Each submodule handles one top-level subcommand.

pub mod config_resolve;
pub mod db;
pub mod export;
pub mod images;
pub mod init;
pub mod jobs;
pub mod logs;
pub mod make;
pub mod mcp;
pub mod serve;
pub mod status;
pub mod templates;
pub mod trash;
pub mod typegen;
pub mod user;

mod cli_types;
mod helpers;

pub use cli_types::{
    BlueprintAction, DbAction, ImagesAction, JobsAction, LogsAction, MakeAction, MigrateAction,
    TemplatesAction, TrashAction, UserAction, parse_key_val,
};
pub use config_resolve::resolve_config_dir;
pub use helpers::load_config_and_sync;
