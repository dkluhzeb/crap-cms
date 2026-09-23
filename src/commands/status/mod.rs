//! `status` command — show project status (collections, globals, migrations, jobs, uploads).
//!
//! With `--check`, runs a best-practice audit on the configuration and project state.

pub(crate) mod check;
mod dispatch;
mod display;
mod migrations;

pub use dispatch::run;
