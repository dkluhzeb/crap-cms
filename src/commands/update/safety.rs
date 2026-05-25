//! Self-update safety checks: refuse to overwrite a distro-managed
//! binary, prompt for confirmation on destructive operations.

use std::path::Path;

use anyhow::{Result, bail};
use dialoguer::Confirm;

use crate::cli;

use super::store;

/// Refuse self-update when the running binary lives outside the user's store
/// (distro-managed install paths like `/usr/bin`, `/opt/...`, `/nix/...`).
/// `--force` bypasses.
pub(super) fn ensure_self_managed(store: &store::Store, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    let Ok(current_exe) = std::env::current_exe() else {
        return Ok(()); // can't figure out the path → don't block the user
    };
    // Resolve symlinks (e.g. our own `current` shim) before checking.
    let resolved = current_exe.canonicalize().unwrap_or(current_exe);

    if store.owns_path(&resolved) {
        return Ok(());
    }

    // Only refuse for paths that smell distro-managed. User-placed binaries
    // under arbitrary paths are allowed through (the install still lands in
    // the store, not over the running binary's location).
    if looks_distro_managed(&resolved) {
        bail!(
            "this binary is at {} — looks like a package-manager install.\n\
             Update via your package manager, or pass `--force` to install into the crap-cms store anyway.",
            resolved.display()
        );
    }
    Ok(())
}

pub(super) fn looks_distro_managed(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("/usr/")
        || s.starts_with("/opt/")
        || s.starts_with("/nix/")
        || s.starts_with("/bin/")
        || s.starts_with("/sbin/")
}

/// Interactive yes/no prompt with a default of "yes". Used by the
/// `update` (no-arg) flow before installing the latest release.
pub(super) fn confirm(prompt: &str) -> Result<bool> {
    Ok(Confirm::with_theme(&cli::crap_theme())
        .with_prompt(prompt)
        .default(true)
        .interact()?)
}

/// Interactive yes/no prompt with a default of "no". Used before
/// destructive operations (e.g. replacing a regular file on `$PATH`)
/// where silently accepting the default could clobber the user's work.
pub(super) fn confirm_destructive(prompt: &str) -> Result<bool> {
    Ok(Confirm::with_theme(&cli::crap_theme())
        .with_prompt(prompt)
        .default(false)
        .interact()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_distro_managed_recognises_system_paths() {
        assert!(looks_distro_managed(Path::new("/usr/bin/crap-cms")));
        assert!(looks_distro_managed(Path::new(
            "/opt/crap-cms/bin/crap-cms"
        )));
        assert!(looks_distro_managed(Path::new(
            "/nix/store/abc/bin/crap-cms"
        )));
    }

    #[test]
    fn looks_distro_managed_ignores_home_paths() {
        assert!(!looks_distro_managed(Path::new(
            "/home/someone/.local/bin/crap-cms"
        )));
        assert!(!looks_distro_managed(Path::new("/tmp/my-install/crap-cms")));
    }
}
