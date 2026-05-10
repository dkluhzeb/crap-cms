//! `crap-cms update` — nvm-style version manager for the crap-cms binary.
//!
//! Subcommands (defined as variants of [`UpdateCmd`] and dispatched in
//! [`dispatch::run`]):
//! - `check` — compare current version to latest released version.
//! - `list` — list available remote tags + mark installed versions.
//! - `install <version>` — download + verify + stage a version in the store.
//! - `use <version>` — switch the `current` symlink to an installed version.
//! - `uninstall <version>` — remove a version from the store.
//! - `where` — print the path of the currently active binary (resolving `current`).
//! - `completions` — generate / uninstall shell completions.
//! - (no subcommand) — shortcut for install-latest + use-latest.
//!
//! ## Layout
//!
//! One file per `crap-cms update <action>` subaction; cross-cutting
//! helpers split by concern:
//! - [`dispatch`]: `UpdateCmd` clap enum + `run` + Windows guard.
//! - [`check`], [`list`], [`install`], [`use_action`], [`uninstall`],
//!   [`where_action`], [`install_latest`]: per-subaction handlers.
//! - [`version`]: tag normalization, semver comparison.
//! - [`safety`]: distro-managed-binary check + interactive confirm.
//! - [`path_lookup`]: `$PATH` resolution + post-`use` warning.
//! - [`cache`], [`checksum`], [`github`], [`platform`], [`store`],
//!   [`completions`]: external integrations.

pub mod cache;
pub mod checksum;
pub mod github;
pub mod platform;
pub mod store;

mod check;
mod completions;
mod dispatch;
mod install;
mod install_latest;
mod list;
mod path_lookup;
mod safety;
mod uninstall;
mod use_action;
mod version;
mod where_action;

pub use dispatch::{UpdateCmd, run};
