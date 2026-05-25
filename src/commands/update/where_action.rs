//! `crap-cms update where` — print the path of the currently active
//! binary (resolving the `current` symlink).

use std::fs;

use anyhow::{Context, Result, bail};

use super::store;

/// Print the active binary's resolved path.
pub(super) fn run_where() -> Result<()> {
    let store = store::Store::default_for_user()?;
    let link = store.current_link();
    if !link.exists() {
        bail!(
            "no active version — the `current` symlink does not exist at {}",
            link.display()
        );
    }
    let resolved = fs::read_link(&link).with_context(|| format!("reading {}", link.display()))?;
    println!("{}", resolved.display());
    Ok(())
}
