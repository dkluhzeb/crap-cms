//! `gen-doc-tables` subcommand: regenerate / verify the generated doc
//! tables — marker-injected regions inside prose pages plus whole
//! generated reference files. Mirrors the other `gen-*` subcommands:
//! write in default mode, diff in `--check` mode (CI gate).

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::drift::{check_drift, workspace_root};
use crap_cms::docgen;

/// Marker-injected regions: (relative doc path, region id, renderer).
fn regions() -> Vec<(&'static str, &'static str, String)> {
    vec![
        (
            "docs/src/admin-ui/guides/slots.md",
            "slots",
            docgen::generate_slots_table(),
        ),
        (
            "docs/src/mcp/overview.md",
            "mcp-reserved-args",
            docgen::generate_mcp_reserved_args_table(),
        ),
        (
            "docs/src/admin-ui/reference/components.md",
            "components-singleton",
            docgen::generate_component_table("singleton"),
        ),
        (
            "docs/src/admin-ui/reference/components.md",
            "components-form-field",
            docgen::generate_component_table("form-field"),
        ),
        (
            "docs/src/admin-ui/reference/components.md",
            "components-enhancer",
            docgen::generate_component_table("enhancer"),
        ),
    ]
}

/// Whole generated files: (relative doc path, content).
fn whole_files() -> Vec<(&'static str, String)> {
    vec![(
        "docs/src/admin-ui/reference/css-variables.md",
        docgen::generate_css_variables_md(),
    )]
}

pub(crate) fn run(check: bool) -> Result<()> {
    let root = workspace_root()?;

    for (rel, id, content) in regions() {
        let path: PathBuf = root.join(rel);
        let doc = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        if check {
            let existing = docgen::region::extract(&doc, id)?;
            if existing != content {
                bail!(
                    "{} region '{id}' is out of sync — run `cargo xtask gen-doc-tables`",
                    path.display()
                );
            }
            println!("{} region '{id}' is up to date", path.display());
        } else {
            let updated = docgen::region::inject(&doc, id, &content)?;
            std::fs::write(&path, updated)
                .with_context(|| format!("failed to write {}", path.display()))?;
            println!("wrote {} region '{id}'", path.display());
        }
    }

    for (rel, content) in whole_files() {
        let path: PathBuf = root.join(rel);

        if check {
            check_drift(&path, &content, "cargo xtask gen-doc-tables")?;
        } else {
            std::fs::write(&path, &content)
                .with_context(|| format!("failed to write {}", path.display()))?;
            println!("wrote {}", path.display());
        }
    }

    Ok(())
}
