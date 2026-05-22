//! `gen-template-doc` subcommand: regenerate / verify
//! `docs/src/admin-ui/reference/template-context.md`.
//!
//! The Markdown reference is rendered from the typed per-page context
//! structs in `src/admin/context/page/` via
//! [`crap_cms::docgen::generate_template_context_md`]. Mirrors the
//! shape of [`crate::gen_lua_types`]: write in default mode, diff in
//! `--check` mode (CI gate).
//!
//! An in-crate `#[test]` at the bottom of `schema_doc.rs` also asserts
//! drift, so local `cargo test` catches staleness even without running
//! the xtask. Both gates call the same render fn — single source of
//! truth.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::drift::{check_drift, workspace_root};

/// Run the `gen-template-doc` subcommand.
pub(crate) fn run(check: bool) -> Result<()> {
    let path = template_doc_path()?;
    let generated = crap_cms::docgen::generate_template_context_md();

    if check {
        check_drift(&path, &generated, "cargo xtask gen-template-doc")
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

/// `<workspace-root>/docs/src/admin-ui/reference/template-context.md`.
fn template_doc_path() -> Result<PathBuf> {
    Ok(workspace_root()?
        .join("docs")
        .join("src")
        .join("admin-ui")
        .join("reference")
        .join("template-context.md"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_doc_matches_repo_copy() {
        // Mirror of the schema_doc::tests::template_context_doc_is_in_sync
        // test, but exercising the same path the xtask `--check`
        // subcommand resolves. Catches drift between what the typed
        // contexts produce and the committed Markdown.
        let generated = crap_cms::docgen::generate_template_context_md();
        let path = template_doc_path().expect("xtask manifest dir resolvable");
        let on_disk = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert_eq!(
            generated, on_disk,
            "template-context.md is out of sync — run `cargo xtask gen-template-doc`"
        );
    }
}
