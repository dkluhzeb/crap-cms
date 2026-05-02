//! `make` command dispatcher.

use anyhow::Result;
use std::path::Path;

use crate::commands::MakeAction;

use super::{
    collection::run_collection, component::run_component, field::run_field, global::run_global,
    hook::run_hook, job::run_job, node::run_node, page::run_page, slot::run_slot, theme::run_theme,
};

/// Dispatch the `make` subcommand to the appropriate handler.
#[cfg(not(tarpaulin_include))]
pub fn run(config_dir: &Path, action: MakeAction) -> Result<()> {
    match action {
        MakeAction::Collection { .. } => run_collection(config_dir, action),
        MakeAction::Global { .. } => run_global(config_dir, action),
        MakeAction::Hook { .. } => run_hook(config_dir, action),
        MakeAction::Job { .. } => run_job(config_dir, action),
        MakeAction::Page { .. } => run_page(config_dir, action),
        MakeAction::Slot { .. } => run_slot(config_dir, action),
        MakeAction::Node { .. } => run_node(config_dir, action),
        MakeAction::Field { .. } => run_field(config_dir, action),
        MakeAction::Theme { .. } => run_theme(config_dir, action),
        MakeAction::Component { .. } => run_component(config_dir, action),
    }
}
