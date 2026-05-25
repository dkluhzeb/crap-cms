//! `crap-cms update use <version>` — switch the `current` symlink to
//! the given (already installed) version, install completions, warn
//! if `$PATH` doesn't pick up the active version.

use anyhow::Result;
use clap::CommandFactory;

use crate::cli;

use super::{
    completions,
    path_lookup::{relink_path_to_store, warn_if_path_misaligned},
    safety::ensure_self_managed,
    store,
    version::normalize_tag,
};

/// Switch the `current` symlink. With `force`, also repoint the binary
/// on `$PATH` at the store so the shell picks up the new version on the
/// next invocation. `yes` skips the confirmation prompt that fires when
/// the PATH binary is a regular file rather than a symlink.
pub(super) fn run_use<C: CommandFactory>(version: &str, yes: bool, force: bool) -> Result<()> {
    let version = normalize_tag(version);
    let store = store::Store::default_for_user()?;
    ensure_self_managed(&store, force)?;
    store.switch_to(&version)?;
    cli::success(&format!("Switched to {version}."));

    completions::install_completions::<C>();

    if force {
        relink_path_to_store(&store, yes)?;
    }
    warn_if_path_misaligned(&store);
    Ok(())
}
