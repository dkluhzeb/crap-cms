//! `crap-cms update uninstall <version>` — remove a version from the
//! store. Also strips installed shell completions if no versions
//! remain (completions follow the tool, not any one version).

use anyhow::Result;

use crate::cli;

use super::{completions, store, version::normalize_tag};

/// Remove a version. Also removes installed shell completions if no
/// versions remain — they follow the tool, not any individual version.
pub(super) fn run_uninstall(version: &str) -> Result<()> {
    let version = normalize_tag(version);
    let store = store::Store::default_for_user()?;
    store.uninstall(&version)?;
    cli::success(&format!("Removed {version}."));

    if store.installed().map(|v| v.is_empty()).unwrap_or(false) {
        completions::uninstall_all_completions();
    }

    Ok(())
}
