//! `make component` — scaffold a custom Web Component file.

use anyhow::{Context as _, Result};
use dialoguer::Input;
use std::path::Path;

use crate::{cli::crap_theme, commands::MakeAction, scaffold};

#[cfg(not(tarpaulin_include))]
pub fn run_component(config_dir: &Path, action: MakeAction) -> Result<()> {
    let MakeAction::Component { tag, force } = action else {
        unreachable!()
    };

    let tag = match tag {
        Some(s) => s,
        None => Input::with_theme(&crap_theme())
            .with_prompt("Tag name (must contain a hyphen, e.g., my-widget)")
            .validate_with(|input: &String| -> Result<(), String> {
                if input.contains('-') {
                    Ok(())
                } else {
                    Err("tag must contain a hyphen".into())
                }
            })
            .interact_text()
            .context("Failed to read tag")?,
    };

    scaffold::make_component(&scaffold::MakeComponentOptions {
        config_dir,
        tag: &tag,
        force,
    })
}
