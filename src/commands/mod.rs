//! CLI command handlers. Each submodule handles one top-level
//! subcommand of the `crap-cms` binary.
//!
//! # Layout — flat-vs-folder rule
//!
//! - **Single-action subcommand → flat file.** `crap-cms fmt`,
//!   `crap-cms init`, `crap-cms work`, etc. live in
//!   `fmt.rs`, `init.rs`, `work.rs`. Currently flat:
//!   `check`, `fmt`, `images`, `init`, `jobs`, `logs`, `mcp`, `trash`,
//!   `typegen`, `work`.
//! - **Multi-action subcommand → folder.** `db/`, `user/`,
//!   `make/`, `serve/`, `update/`, `templates/`, `bench/`,
//!   `export/`, `status/` each have multiple `crap-cms <cmd>
//!   <action>` subactions. Folder layout is "one file per
//!   `<cmd> <action>` subaction" plus `mod.rs` (re-exports only)
//!   and `dispatch.rs` (matches the action enum to a handler);
//!   sometimes a `helpers.rs` for cross-action utilities.
//!
//! When adding a new command, default to a flat file; promote to a
//! folder the first time you add a second action. The `cli.rs`,
//! `helpers.rs`, `types.rs`, and `resolve_config.rs` files at this
//! module's root are *not* commands — they're cross-cutting
//! infrastructure shared by every subcommand (see "Cross-cutting
//! helpers" below).
//!
//! # Entry-point convention
//!
//! Every subcommand exposes a `pub fn run(...)` (or `pub async fn
//! run(...)`) reached by `main.rs` via `commands::<sub>::run`.
//! Action enums for clap (`UserAction`, `MigrateAction`, …) live
//! in `types.rs` and are re-exported at the crate level so
//! `main.rs` parses them once and hands the resolved variant to
//! `run`.
//!
//! The clap root itself ([`Cli`] / [`Command`]) lives in `cli.rs`,
//! not in `main.rs`: the binary only calls `Cli::parse()` and
//! dispatches, while the library can introspect the same
//! [`clap::Command`] tree (the `cli_reference` MCP tool derives its
//! whole answer from it).
//!
//! # Cross-cutting helpers
//!
//! - [`resolve_config_dir`] walks the CWD upwards looking for
//!   `crap.toml` (or honors `--config` / `CRAP_CONFIG_DIR`).
//!   Every command that needs a config calls it first.
//! - [`open_project`] loads `crap.toml`, takes the instance lock
//!   shared, opens the DB pool, and runs schema sync. Used by every
//!   command that reads or writes data.
//! - [`UpdateCmd`] is the typed clap subcommand for `crap-cms
//!   update *` — exposed so `main.rs` can wire it directly.
//!
//! # Visibility convention
//!
//! Per-subcommand helper fns are `pub(super)` — visible only
//! within their own subcommand subdir. A new helper used inside
//! one subdir only defaults to `pub(super)`; widen it only when a
//! caller outside that subdir appears.

pub mod bench;
pub mod check;
pub mod db;
pub mod export;
pub mod fmt;
pub mod images;
pub mod init;
pub mod jobs;
pub mod logs;
pub mod make;
pub mod mcp;
pub mod resolve_config;
pub mod serve;
pub mod status;
pub mod templates;
pub mod trash;
pub mod typegen;
pub mod update;
pub mod user;
pub mod work;

mod cli;
mod helpers;
mod types;

pub use cli::{Cli, Command};
pub use helpers::{
    Project, cli_find, cli_infra, load_config, load_config_for_recovery, open_project,
};
pub use resolve_config::resolve_config_dir;
pub use types::{
    BenchAction, BlueprintAction, DbAction, ImagesAction, JobsAction, LogsAction, MakeAction,
    MigrateAction, TemplatesAction, TrashAction, TypegenAction, UserAction, parse_key_val,
};
pub use update::UpdateCmd;

// User-management library entry points + their `*Params` structs.
// Reached from integration tests in `tests/` and from `init.rs`'s
// first-user prompt. Items reached externally ≥2 times live at
// `commands::*` rather than `commands::user::*` so callers don't
// repeat the deep path.
pub use user::{
    UserChangePasswordParams, UserCreateParams, UserDeleteParams, UserLookup, user_account_action,
    user_change_password, user_create, user_delete, user_list,
};
