//! `crap-cms update install <version>` — download + verify + stage a
//! version in the local store.
//!
//! The download is written straight into a [`store::StagedBinary`] (a
//! partial file inside the destination version directory), verified against
//! the release's `SHA256SUMS` and atomically renamed into place — see the
//! `store` module docs.

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::cli;

use super::{checksum, github, platform, store, version::normalize_tag};

/// Refuse a tag that is not a published release, listing a few that are.
fn ensure_published(version: &str) -> Result<()> {
    let releases = github::list_releases(github::DEFAULT_REPO)?;
    if releases.iter().any(|r| r.tag_name == version) {
        return Ok(());
    }

    let mut msg = format!("version {version} is not a published release.");
    let tags: Vec<String> = releases
        .iter()
        .take(10)
        .map(|r| r.tag_name.clone())
        .collect();
    if !tags.is_empty() {
        msg.push_str("\n\nAvailable versions:\n  ");
        msg.push_str(&tags.join("\n  "));
    }
    msg.push_str("\n\nRun `crap-cms update list` to see the full list.");

    bail!(msg);
}

/// Download the platform asset into the store's staging file, verify it
/// against `SHA256SUMS` and commit it as the version's binary.
fn download_and_install(store: &store::Store, version: &str) -> Result<PathBuf> {
    let asset = platform::asset_name()?;
    let sums = github::fetch_sha256sums(github::DEFAULT_REPO, version)?;
    let expected = checksum::expected_hex_required(&sums, &asset)?;

    let staged = store.stage_binary(version)?;

    cli::info(&format!("Downloading {version}/{asset}..."));
    github::download_asset(github::DEFAULT_REPO, version, &asset, staged.path())?;

    cli::info("Verifying SHA256...");
    staged.commit(&expected)
}

/// Download + verify + install a specific version.
///
/// Stages the binary in the version store only — `<store>/versions/<version>/`
/// is the only directory it writes. The running
/// binary and the one on `$PATH` are untouched until `update use`, so a
/// distro-managed install is no reason to refuse here; `update use` carries
/// that guard.
pub(super) fn run_install(version: &str, reinstall: bool) -> Result<()> {
    let version = normalize_tag(version)?;
    let store = store::Store::default_for_user()?;

    if !reinstall && store.installed()?.contains(&version) {
        cli::info(&format!(
            "{version} is already installed. Use `--reinstall` to redownload, or `crap-cms update use {version}` to activate it."
        ));
        return Ok(());
    }

    // Verify the tag exists in the remote release list before we hit any
    // download URL — gives the user a helpful "did you mean…" instead of a
    // raw HTTP 404 when they typo'd the version.
    ensure_published(&version)?;

    let installed_path = download_and_install(&store, &version)?;
    cli::success(&format!(
        "Installed {version} at {}",
        installed_path.display()
    ));

    // Help the user discover the next step. `install` stages only; the user
    // has to explicitly `use` a version to activate it (rustup-style).
    match store.active_version() {
        Some(active) if active == version => {
            // Already active (e.g., `--reinstall` of the current version) —
            // no next step needed.
        }
        Some(active) => {
            cli::hint(&format!(
                "Active version is still {active}. Run `crap-cms update use {version}` to switch."
            ));
        }
        None => {
            cli::hint(&format!(
                "No version is active yet. Run `crap-cms update use {version}` to activate it."
            ));
        }
    }
    Ok(())
}
