//! `crap-cms update list` — print all remote release tags, marking
//! installed and active.

use anyhow::Result;

use super::{github, store};

/// Print all remote release tags, marking installed and active.
pub(super) fn run_list() -> Result<()> {
    let releases = github::list_releases(github::DEFAULT_REPO)?;
    let store = store::Store::default_for_user().ok();
    let installed = store
        .as_ref()
        .and_then(|s| s.installed().ok())
        .unwrap_or_default();
    let active = store.as_ref().and_then(|s| s.active_version());

    for release in releases {
        let is_installed = installed.contains(&release.tag_name);
        let is_active = active.as_deref() == Some(&release.tag_name);
        let marker = match (is_active, is_installed) {
            (true, _) => "*",
            (false, true) => " ",
            _ => " ",
        };
        let suffix = match (is_active, is_installed, release.prerelease) {
            (true, _, true) => " (active, prerelease)",
            (true, _, false) => " (active)",
            (false, true, true) => " (installed, prerelease)",
            (false, true, false) => " (installed)",
            (_, _, true) => " (prerelease)",
            _ => "",
        };
        println!("{marker} {}{suffix}", release.tag_name);
    }
    Ok(())
}
