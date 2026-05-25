//! `crap-cms update check` — compare current to latest, exit code 1
//! if a newer release is available.

use anyhow::Result;
use chrono::Utc;

use crate::cli;

use super::{
    cache, github,
    version::{current_version, is_newer},
};

/// Compare current crate version to the latest release tag.
pub(super) fn run_check() -> Result<()> {
    let current = current_version();
    let latest = github::latest_tag(github::DEFAULT_REPO)?;
    let now = Utc::now();

    // Write the cache for the startup nudge regardless of the comparison result.
    if let Some(path) = cache::default_path() {
        let _ = cache::write_at(
            &path,
            &cache::UpdateCache {
                checked_at: now,
                latest: latest.clone(),
            },
        );
    }

    if is_newer(&latest, &current) {
        cli::info(&format!(
            "Newer release available: {latest} (current: {current})"
        ));
        cli::hint("Run `crap-cms update` to install and switch.");
        std::process::exit(1);
    }

    cli::success(&format!("Up to date ({current})."));
    Ok(())
}
