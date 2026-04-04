//! `make job` — scaffold a new job definition.

use anyhow::{Context as _, Result};
use dialoguer::Input;
use std::path::Path;

use crate::{cli::crap_theme, commands::MakeAction, scaffold};

/// Handle the `make job` subcommand — resolve slug interactively if missing.
#[cfg(not(tarpaulin_include))]
pub fn run_job(config_dir: &Path, action: MakeAction) -> Result<()> {
    let MakeAction::Job {
        slug,
        schedule,
        queue,
        retries,
        timeout,
        force,
    } = action
    else {
        unreachable!()
    };

    let slug = match slug {
        Some(s) => s,
        None => Input::with_theme(&crap_theme())
            .with_prompt("Job slug")
            .validate_with(|input: &String| -> Result<(), String> {
                scaffold::validate_slug(input).map_err(|e| e.to_string())
            })
            .interact_text()
            .context("Failed to read job slug")?,
    };

    scaffold::make_job(
        config_dir,
        &slug,
        schedule.as_deref(),
        queue.as_deref(),
        retries,
        timeout,
        force,
    )
}
