//! `$PATH` lookup helpers — used after `update use` to verify the
//! user's shell will actually pick up the version that was just
//! activated.

use std::{fs, path::PathBuf};

use crate::cli;

use super::store;

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
    cli::hint("(Or remove the conflicting binary and re-run `scripts/install.sh`.)");
}
