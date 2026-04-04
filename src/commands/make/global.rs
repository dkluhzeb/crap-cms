//! `make global` — scaffold a new global definition.

use anyhow::{Context as _, Result};
use dialoguer::Input;
use std::path::Path;

use crate::{cli::crap_theme, commands::MakeAction, scaffold};

/// Handle the `make global` subcommand — resolve slug interactively if missing.
#[cfg(not(tarpaulin_include))]
pub fn run_global(config_dir: &Path, action: MakeAction) -> Result<()> {
    let MakeAction::Global {
        slug,
        fields,
        force,
    } = action
    else {
        unreachable!()
    };

    let slug = match slug {
        Some(s) => s,
        None => Input::with_theme(&crap_theme())
            .with_prompt("Global slug")
            .validate_with(|input: &String| -> Result<(), String> {
                scaffold::validate_slug(input).map_err(|e| e.to_string())
            })
            .interact_text()
            .context("Failed to read global slug")?,
    };

    let parsed = fields
        .map(|s| scaffold::parse_fields_shorthand(&s))
        .transpose()?;

    scaffold::make_global(config_dir, &slug, parsed.as_deref(), force)
}
