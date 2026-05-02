//! `make slot` — scaffold a slot-widget HBS file.

use anyhow::{Context as _, Result};
use dialoguer::Input;
use std::path::Path;

use crate::{cli::crap_theme, commands::MakeAction, scaffold};

#[cfg(not(tarpaulin_include))]
pub fn run_slot(config_dir: &Path, action: MakeAction) -> Result<()> {
    let MakeAction::Slot { slot, file, force } = action else {
        unreachable!()
    };

    let slot = match slot {
        Some(s) => s,
        None => Input::with_theme(&crap_theme())
            .with_prompt("Slot name (e.g., dashboard_widgets, page_header_actions)")
            .validate_with(|input: &String| -> Result<(), String> {
                scaffold::validate_slug(input).map_err(|e| e.to_string())
            })
            .interact_text()
            .context("Failed to read slot name")?,
    };

    scaffold::make_slot(&scaffold::MakeSlotOptions {
        config_dir,
        slot: &slot,
        file: file.as_deref(),
        force,
    })
}
