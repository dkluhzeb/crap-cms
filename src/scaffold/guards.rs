//! Shared guards used by `make_*` generators. Lifting the
//! same-shape overwrite check to a single helper keeps the error
//! message uniform across every subcommand and makes a typo at one
//! site impossible.

use std::path::Path;

use anyhow::{Result, bail};

/// Refuse to overwrite an existing file unless `force` is set.
///
/// Emits the standard "File '...' already exists -- use --force to
/// overwrite" message every `make_*` subcommand uses.
pub(crate) fn refuse_file_overwrite(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "File '{}' already exists -- use --force to overwrite",
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_when_file_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("new.lua");
        assert!(refuse_file_overwrite(&p, false).is_ok());
    }

    #[test]
    fn refuses_existing_file_without_force() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("exists.lua");
        std::fs::write(&p, "x").unwrap();
        let err = refuse_file_overwrite(&p, false).unwrap_err().to_string();
        assert!(err.contains("already exists"), "{err}");
    }

    #[test]
    fn allows_existing_file_with_force() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("exists.lua");
        std::fs::write(&p, "x").unwrap();
        assert!(refuse_file_overwrite(&p, true).is_ok());
    }
}
