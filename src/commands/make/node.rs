//! `make node` — scaffold a custom richtext-node Lua snippet.

use anyhow::{Context as _, Result};
use dialoguer::Input;
use std::path::Path;

use crate::{cli::crap_theme, commands::MakeAction, scaffold};

#[cfg(not(tarpaulin_include))]
pub fn run_node(config_dir: &Path, action: MakeAction) -> Result<()> {
    let MakeAction::Node {
        name,
        inline,
        force,
    } = action
    else {
        unreachable!()
    };

    let name = match name {
        Some(s) => s,
        None => Input::with_theme(&crap_theme())
            .with_prompt("Richtext node name (e.g., cta, mention)")
            .validate_with(|input: &String| -> Result<(), String> {
                scaffold::validate_slug(input).map_err(|e| e.to_string())
            })
            .interact_text()
            .context("Failed to read node name")?,
    };

    scaffold::make_node(&scaffold::MakeNodeOptions {
        config_dir,
        name: &name,
        inline,
        force,
    })
}
