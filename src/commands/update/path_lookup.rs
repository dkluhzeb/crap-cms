//! `$PATH` lookup helpers — used after `update use` to verify the
//! user's shell will actually pick up the version that was just
//! activated.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};

use crate::cli;

use super::{
    safety::{confirm_destructive, looks_distro_managed},
    store,
};

/// Resolve the first `crap-cms` executable on `$PATH`, matching what the user's
/// shell would pick when they type `crap-cms`.
fn resolve_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if !candidate.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = fs::metadata(&candidate)
                && meta.permissions().mode() & 0o111 != 0
            {
                return Some(candidate);
            }
        }
        #[cfg(not(unix))]
        {
            return Some(candidate);
        }
    }
    None
}

/// After `update use` flips the internal `current` symlink, the user's shell
/// may still resolve `crap-cms` to an older binary elsewhere on `$PATH` —
/// e.g. a `/usr/local/bin/crap-cms` from a manual install, or no shim at all.
/// Surface this as an explicit warning so "Switched to X" never misleads.
pub(super) fn warn_if_path_misaligned(store: &store::Store) {
    let Some(active) = store.active_version() else {
        return;
    };
    let expected = store.version_path(&active);
    let expected_canonical = expected.canonicalize().unwrap_or(expected.clone());

    let Some(on_path) = resolve_on_path("crap-cms") else {
        cli::warning("`crap-cms` is not on your PATH.");
        cli::hint(&format!(
            "Link the shim:  ln -sfn {} ~/.local/bin/crap-cms",
            store.current_link().display()
        ));
        cli::hint("Then make sure `~/.local/bin` is on your PATH.");
        return;
    };

    let on_path_canonical = on_path.canonicalize().unwrap_or(on_path.clone());
    if on_path_canonical == expected_canonical {
        return; // all wired up — the shell will pick up the active version.
    }

    cli::warning(&format!(
        "`crap-cms` on PATH resolves to {} — not the version you just activated.",
        on_path.display()
    ));
    cli::hint(&format!(
        "Point your shim at the store:  ln -sfn {} ~/.local/bin/crap-cms",
        store.current_link().display()
    ));
    cli::hint(
        "(Or remove the conflicting binary and re-run `scripts/install.sh`, or pass `--force`.)",
    );
}

/// Compare the running binary with the store's active version. If they
/// differ, emit a warning so the user knows "Already on the latest
/// release" (computed from the *running* binary's compile-time version)
/// doesn't reflect what their shell will actually run next time.
///
/// Returns `true` if a mismatch was reported — callers can use this to
/// reframe a follow-up "remote already-latest" message so it doesn't
/// contradict the warning.
pub(super) fn warn_if_running_binary_mismatches_store(store: &store::Store) -> bool {
    let Ok(running) = std::env::current_exe() else {
        return false;
    };

    mismatch_inner(store, &running)
}

/// Core of [`warn_if_running_binary_mismatches_store`] with the running
/// binary path lifted out so tests can supply a controlled value.
fn mismatch_inner(store: &store::Store, running: &Path) -> bool {
    let running_canonical = running
        .canonicalize()
        .unwrap_or_else(|_| running.to_path_buf());
    let current_canonical = store.current_link().canonicalize().ok();

    if let Some(target) = &current_canonical
        && &running_canonical == target
    {
        return false; // aligned — silent
    }

    if store.owns_path(&running_canonical) {
        // Running binary is in the store, just not the active version.
        cli::warning(&format!(
            "Running binary {} is in the store but is not the currently-active version.",
            running.display()
        ));
        cli::hint("Switch to it explicitly with `crap-cms update use <version>`.");
    } else {
        // Running binary is somewhere outside the store entirely.
        cli::warning(&format!(
            "`crap-cms` from your shell ({}) is not the store-managed binary.",
            running.display()
        ));

        if let Some(target) = &current_canonical {
            cli::hint(&format!("Store currently has {} active.", target.display()));
        }

        cli::hint("Run `crap-cms update use --force <version>` to repoint your PATH at the store.");
    }

    true
}

/// Repoint the `$PATH` binary at the store's `current` symlink. Called by
/// `update use --force` after the active version was switched, so the
/// user's shell picks up the new version without manual symlink fiddling.
///
/// - Symlinks (any non-store target) are replaced silently.
/// - Regular files prompt for confirmation unless `yes` is set, since
///   removing them could clobber a legitimate other-tool install (e.g.
///   `cargo install` output sitting at `~/.local/bin/crap-cms`).
/// - Distro-managed locations (`/usr/bin`, `/opt`, `/nix/store`, …) refuse
///   even with `--force` — those belong to the system package manager.
pub(super) fn relink_path_to_store(store: &store::Store, yes: bool) -> Result<()> {
    let Some(on_path) = resolve_on_path("crap-cms") else {
        cli::warning("`crap-cms` is not on your PATH; cannot relink.");
        cli::hint(&format!(
            "Link the shim manually:  ln -sfn {} ~/.local/bin/crap-cms",
            store.current_link().display()
        ));
        return Ok(());
    };

    relink_inner(store, &on_path, yes)
}

/// Core of `relink_path_to_store` with the `$PATH` resolution lifted out
/// so tests can supply a controlled target path.
fn relink_inner(store: &store::Store, on_path: &Path, yes: bool) -> Result<()> {
    let Some(active) = store.active_version() else {
        return Ok(());
    };
    let expected_canonical = store
        .version_path(&active)
        .canonicalize()
        .unwrap_or_else(|_| store.version_path(&active));

    let on_path_canonical = on_path
        .canonicalize()
        .unwrap_or_else(|_| on_path.to_path_buf());

    if on_path_canonical == expected_canonical {
        return Ok(());
    }

    if looks_distro_managed(on_path) || looks_distro_managed(&on_path_canonical) {
        bail!(
            "`crap-cms` on PATH is at {} — looks distro-managed; refusing to relink even with --force.\n\
             Remove it via your package manager (or by hand) and re-run.",
            on_path.display()
        );
    }

    let metadata = fs::symlink_metadata(on_path)
        .with_context(|| format!("inspecting {}", on_path.display()))?;
    let is_symlink = metadata.file_type().is_symlink();

    if !is_symlink
        && !yes
        && !confirm_destructive(&format!(
            "Replace regular file {} with a symlink to the store?",
            on_path.display()
        ))?
    {
        cli::warning("Aborted relinking; PATH still points at the old binary.");
        return Ok(());
    }

    fs::remove_file(on_path).with_context(|| format!("removing {}", on_path.display()))?;

    #[cfg(unix)]
    std::os::unix::fs::symlink(store.current_link(), on_path).with_context(|| {
        format!(
            "symlinking {} → {}",
            on_path.display(),
            store.current_link().display()
        )
    })?;

    cli::success(&format!(
        "Relinked {} → {}.",
        on_path.display(),
        store.current_link().display()
    ));

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Build a store at `root` with a single fake version `v0.1.0`. Returns
    /// the store and the version-binary path so tests can use it for symlinks.
    fn build_fake_store(root: PathBuf) -> (store::Store, PathBuf) {
        let store = store::Store::at(root);
        let vdir = store.versions_dir().join("v0.1.0");

        fs::create_dir_all(&vdir).unwrap();

        let bin = vdir.join("crap-cms");
        fs::write(&bin, b"#!/bin/sh\necho fake\n").unwrap();

        // Make the version "active" by pointing `current` at it.
        store.switch_to("v0.1.0").unwrap();

        (store, bin)
    }

    #[test]
    fn relink_inner_noop_when_already_aligned() {
        let tmp = tempdir().unwrap();
        let (store, _bin) = build_fake_store(tmp.path().to_path_buf());

        // PATH binary pointing at `current` (canonicalizes to the active version).
        let on_path = tmp.path().join("crap-cms");
        std::os::unix::fs::symlink(store.current_link(), &on_path).unwrap();

        relink_inner(&store, &on_path, false).unwrap();

        // Still a symlink to current_link — unchanged.
        assert!(
            fs::symlink_metadata(&on_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&on_path).unwrap(), store.current_link());
    }

    #[test]
    fn relink_inner_replaces_stale_symlink_silently() {
        let tmp = tempdir().unwrap();
        let (store, _bin) = build_fake_store(tmp.path().to_path_buf());

        // Symlink pointing somewhere else (a "stale shim").
        let other = tmp.path().join("other-binary");
        fs::write(&other, b"x").unwrap();
        let on_path = tmp.path().join("crap-cms");
        std::os::unix::fs::symlink(&other, &on_path).unwrap();

        relink_inner(&store, &on_path, false).unwrap();

        assert!(
            fs::symlink_metadata(&on_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&on_path).unwrap(), store.current_link());
    }

    #[test]
    fn relink_inner_replaces_regular_file_when_yes_set() {
        let tmp = tempdir().unwrap();
        let (store, _bin) = build_fake_store(tmp.path().to_path_buf());

        // Regular file (e.g. `cargo install` output).
        let on_path = tmp.path().join("crap-cms");
        fs::write(&on_path, b"old-binary").unwrap();
        assert!(
            !fs::symlink_metadata(&on_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );

        relink_inner(&store, &on_path, true).unwrap();

        assert!(
            fs::symlink_metadata(&on_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_link(&on_path).unwrap(), store.current_link());
    }

    #[test]
    fn relink_inner_refuses_distro_managed_paths() {
        let tmp = tempdir().unwrap();
        let (store, _bin) = build_fake_store(tmp.path().to_path_buf());

        // Path that starts with "/usr/" — would look distro-managed.
        let on_path = Path::new("/usr/bin/crap-cms");
        let err = relink_inner(&store, on_path, true).unwrap_err();

        assert!(
            err.to_string().contains("looks distro-managed"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn mismatch_inner_silent_when_running_is_active_version() {
        let tmp = tempdir().unwrap();
        let (store, bin) = build_fake_store(tmp.path().to_path_buf());

        // Running binary == the active version's binary.
        assert!(!mismatch_inner(&store, &bin));
    }

    #[test]
    fn mismatch_inner_warns_for_outside_binary() {
        let tmp = tempdir().unwrap();
        let (store, _bin) = build_fake_store(tmp.path().to_path_buf());

        // A binary somewhere outside the store entirely.
        let outside = tmp.path().join("local-bin").join("crap-cms");
        fs::create_dir_all(outside.parent().unwrap()).unwrap();
        fs::write(&outside, b"dev-build").unwrap();

        assert!(mismatch_inner(&store, &outside));
    }

    #[test]
    fn mismatch_inner_warns_for_in_store_but_inactive_version() {
        let tmp = tempdir().unwrap();
        let (store, _bin) = build_fake_store(tmp.path().to_path_buf());

        // Install a second version and keep the first one active.
        let other_vdir = store.versions_dir().join("v0.2.0");
        fs::create_dir_all(&other_vdir).unwrap();
        let other_bin = other_vdir.join("crap-cms");
        fs::write(&other_bin, b"#!/bin/sh\necho v2\n").unwrap();

        // v0.1.0 is still active (via build_fake_store); running v0.2.0.
        assert!(mismatch_inner(&store, &other_bin));
    }
}
