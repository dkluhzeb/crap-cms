//! `gen-wire-doc` subcommand: regenerate / verify
//! `docs/src/reference/operation-options.md`.
//!
//! The Markdown reference is rendered from the single-source wire model
//! via [`crap_cms::docgen::generate_wire_reference_md`]. Mirrors the shape
//! of [`crate::gen_template_doc`]: write in default mode, diff in `--check`
//! mode (CI gate). An in-crate `#[test]` in `wire_doc.rs` asserts the same
//! sync, so plain `cargo test` catches staleness too.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::drift::{check_drift, workspace_root};

/// Run the `gen-wire-doc` subcommand.
pub(crate) fn run(check: bool) -> Result<()> {
    let path = wire_doc_path()?;
    let generated = crap_cms::docgen::generate_wire_reference_md();

    if check {
        check_drift(&path, &generated, "cargo xtask gen-wire-doc")
    } else {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&path, &generated)
            .with_context(|| format!("failed to write {}", path.display()))?;
        println!("wrote {}", path.display());
        Ok(())
    }
}

/// `<workspace-root>/docs/src/reference/operation-options.md`.
fn wire_doc_path() -> Result<PathBuf> {
    Ok(workspace_root()?
        .join("docs")
        .join("src")
        .join("reference")
        .join("operation-options.md"))
}
