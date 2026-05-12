//! `crap-cms update` (no subcommand) — install latest + switch to it.

use anyhow::Result;
use clap::CommandFactory;

use crate::cli;

use super::{
    github,
    install::run_install,
    safety::confirm,
    use_action::run_use,
    version::{current_version, is_newer},
};

/// `crap-cms update` (no args): install latest + switch to it.
pub(super) fn run_update_latest<C: CommandFactory>(yes: bool, force: bool) -> Result<()> {
    let latest = github::latest_tag(github::DEFAULT_REPO)?;
    let current = current_version();

    if !is_newer(&latest, &current) {
        cli::success(&format!("Already on the latest release ({current})."));
        return Ok(());
    }

    cli::info(&format!("Current: {current}"));
    cli::info(&format!("Latest:  {latest}"));

    if !yes && !confirm(&format!("Install {latest} and switch to it?"))? {
        cli::warning("Aborted.");
        return Ok(());
    }

    run_install(&latest, false, force)?;
    run_use::<C>(&latest, yes, force)?;
    Ok(())
}
