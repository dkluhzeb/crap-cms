//! `make` command dispatcher.

use anyhow::Result;
use std::path::Path;

use crate::commands::MakeAction;

use super::{collection::run_collection, global::run_global, hook::run_hook, job::run_job};

/// Dispatch the `make` subcommand to the appropriate handler.
#[cfg(not(tarpaulin_include))]
pub fn run(config_dir: &Path, action: MakeAction) -> Result<()> {
    match action {
        MakeAction::Collection { .. } => run_collection(config_dir, action),
        MakeAction::Global { .. } => run_global(config_dir, action),
        MakeAction::Hook { .. } => run_hook(config_dir, action),
        MakeAction::Job { .. } => run_job(config_dir, action),
    }
}
