//! `make theme` — scaffold a theme starter CSS file.

use anyhow::{Context as _, Result};
use dialoguer::Input;
use std::path::Path;

use crate::{cli::crap_theme, commands::MakeAction, scaffold};

#[cfg(not(tarpaulin_include))]
pub fn run_theme(config_dir: &Path, action: MakeAction) -> Result<()> {
    let MakeAction::Theme { name, force } = action else {
        unreachable!()
    };

    let name = match name {
        Some(s) => s,
        None => Input::with_theme(&crap_theme())
            .with_prompt("Theme name (e.g., acme)")
            .validate_with(|input: &String| -> Result<(), String> {
                scaffold::validate_slug(input).map_err(|e| e.to_string())
            })
            .interact_text()
            .context("Failed to read theme name")?,
    };

    scaffold::make_theme(&scaffold::MakeThemeOptions {
        config_dir,
        name: &name,
        force,
    })
}
