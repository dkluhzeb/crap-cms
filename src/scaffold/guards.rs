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
