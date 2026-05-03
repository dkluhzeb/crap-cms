//! `make field` — scaffold a per-field render template + plugin + Web Component.

use anyhow::{Context as _, Result};
use dialoguer::Input;
use std::path::Path;

use crate::{cli::crap_theme, commands::MakeAction, scaffold};

#[cfg(not(tarpaulin_include))]
pub fn run_field(config_dir: &Path, action: MakeAction) -> Result<()> {
    let MakeAction::Field {
        name,
        base_type,
        force,
    } = action
    else {
        unreachable!()
    };

    let name = match name {
        Some(s) => s,
        None => Input::with_theme(&crap_theme())
            .with_prompt("Field name (e.g., rating, color_picker)")
            .validate_with(|input: &String| -> Result<(), String> {
                scaffold::validate_slug(input).map_err(|e| e.to_string())
            })
            .interact_text()
            .context("Failed to read field name")?,
    };

    scaffold::make_field(&scaffold::MakeFieldOptions {
        config_dir,
        name: &name,
        base_type: base_type.as_deref(),
        force,
    })
}
