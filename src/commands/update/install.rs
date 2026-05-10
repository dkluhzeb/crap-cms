//! `crap-cms update install <version>` — download + verify + stage a
//! version in the local store. Includes the `ScratchDir` RAII helper
//! used for the temporary download directory.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::cli;

use super::{
    checksum, github, platform, safety::ensure_self_managed, store, version::normalize_tag,
};

/// Download + verify + install a specific version.
pub(super) fn run_install(version: &str, reinstall: bool, force: bool) -> Result<()> {
    let version = normalize_tag(version);
    let store = store::Store::default_for_user()?;

    // Guard: running binary is outside the store → refuse unless --force.
    ensure_self_managed(&store, force)?;

    if !reinstall && store.installed()?.contains(&version) {
        cli::info(&format!(
            "{version} is already installed. Use `--reinstall` to redownload, or `crap-cms update use {version}` to activate it."
        ));
        return Ok(());
    }

    // Verify the tag exists in the remote release list before we hit any
    // download URL — gives the user a helpful "did you mean…" instead of a
    // raw HTTP 404 when they typo'd the version.
    let releases = github::list_releases(github::DEFAULT_REPO)?;
    if !releases.iter().any(|r| r.tag_name == version) {
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

    let asset = platform::asset_name()?;
    let tmp_dir = ScratchDir::new()?;
    let tmp_bin = tmp_dir.path().join(&asset);

    cli::info(&format!("Downloading {version}/{asset}..."));
    github::download_asset(github::DEFAULT_REPO, &version, &asset, &tmp_bin)?;

    cli::info("Verifying SHA256...");
    let sums = github::fetch_sha256sums(github::DEFAULT_REPO, &version)?;
    checksum::verify_against_manifest(&tmp_bin, &sums, &asset)?;

    let installed_path = store.install_binary(&version, &tmp_bin)?;
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

/// Tiny scoped tempdir — we don't depend on the `tempfile` crate at runtime.
/// Cleans up on drop (best-effort).
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new() -> Result<Self> {
        let base = std::env::temp_dir();
        let suffix = nanoid::nanoid!(12);
        let dir = base.join(format!("crap-cms-update-{}-{suffix}", std::process::id()));
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { path: dir })
    }
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
