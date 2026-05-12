//! `crap-cms update` (no subcommand) — install latest + switch to it.

use anyhow::Result;
use clap::CommandFactory;

use crate::cli;

use super::{
    github,
    install::run_install,
    path_lookup::warn_if_running_binary_mismatches_store,
    safety::confirm,
    store,
    use_action::run_use,
    version::{current_version, is_newer},
};

/// `crap-cms update` (no args): install latest + switch to it.
pub(super) fn run_update_latest<C: CommandFactory>(yes: bool, force: bool) -> Result<()> {
    // First: surface a running-binary vs. store mismatch. "Already on the
    // latest release" is computed from the running binary's compile-time
    // version, which is misleading when the shell is resolving `crap-cms`
    // to something outside the store (e.g. a `cargo install --path .` dev
    // build shadowing the shim).
    let store = store::Store::default_for_user()?;
    let mismatched = warn_if_running_binary_mismatches_store(&store);

    let latest = github::latest_tag(github::DEFAULT_REPO)?;
    let current = current_version();

    if !is_newer(&latest, &current) {
        // When mismatched, downgrade the "all good" framing — the user
        // needs to act on the warning above, not see a success line.
        if mismatched {
            cli::info(&format!(
                "Remote: already on the latest release ({latest})."
            ));
        } else {
            cli::success(&format!("Already on the latest release ({current})."));
        }
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
