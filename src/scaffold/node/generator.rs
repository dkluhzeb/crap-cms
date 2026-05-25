//! `make node` -- scaffold a custom richtext-node registration.
//!
//! Writes `<config_dir>/lua/richtext_nodes/<name>.lua` containing the
//! `crap.richtext.register_node(...)` call. Doesn't auto-modify
//! `init.lua` (we don't want a destructive AST rewrite); instead the
//! command prints the one-line `require()` to add manually.

use std::{fs, path::Path};

use anyhow::{Context as _, Result};
use serde::Serialize;

use crate::{
    cli,
    scaffold::{guards::refuse_file_overwrite, paths, render, to_title_case, validate_slug},
};

#[derive(Serialize)]
struct NodeCtx<'a> {
    name: &'a str,
    label: String,
    kind: &'static str,
    inline_str: &'static str,
}

/// Options for `make_node`.
pub struct MakeNodeOptions<'a> {
    pub config_dir: &'a Path,
    pub name: &'a str,
    pub inline: bool,
    pub force: bool,
}

/// Scaffold the richtext-node Lua snippet.
///
/// # Errors
///
/// Returns an error if the name is invalid, the file already exists without
/// `--force`, or writing the file fails.
pub fn make_node(opts: &MakeNodeOptions) -> Result<()> {
    validate_slug(opts.name)?;

    let dir = paths::richtext_nodes_dir(opts.config_dir);
    fs::create_dir_all(&dir).context("Failed to create lua/richtext_nodes/ directory")?;

    let file_path = dir.join(format!("{}.lua", opts.name));
    refuse_file_overwrite(&file_path, opts.force)?;

    let lua = render_node_lua(opts)?;
    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    cli::success(&format!("Created {}", file_path.display()));
    cli::info(&format!(
        "Add this line to your init.lua to load the registration:\n\n  require(\"lua.richtext_nodes.{}\")",
        opts.name,
    ));

    Ok(())
}

fn render_node_lua(opts: &MakeNodeOptions) -> Result<String> {
    render::render(
        "node",
        &NodeCtx {
            name: opts.name,
            label: to_title_case(opts.name),
            kind: if opts.inline { "inline" } else { "block-level" },
            inline_str: if opts.inline { "true" } else { "false" },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_block_node_default() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_node(&MakeNodeOptions {
            config_dir: tmp.path(),
            name: "cta",
            inline: false,
            force: false,
        })
        .unwrap();
        let file = tmp.path().join("lua/richtext_nodes/cta.lua");
        assert!(file.exists());
        let body = fs::read_to_string(&file).unwrap();
        assert!(body.contains(r#"crap.richtext.register_node("cta""#));
        assert!(body.contains("inline = false"));
        assert!(body.contains("Cta") || body.contains("cta"));
    }

    #[test]
    fn writes_inline_node_when_requested() {
        let tmp = tempfile::tempdir().expect("tempdir");
        make_node(&MakeNodeOptions {
            config_dir: tmp.path(),
            name: "mention",
            inline: true,
            force: false,
        })
        .unwrap();
        let body = fs::read_to_string(tmp.path().join("lua/richtext_nodes/mention.lua")).unwrap();
        assert!(body.contains("inline = true"));
    }

    #[test]
    fn refuses_to_overwrite_without_force() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let opts = MakeNodeOptions {
            config_dir: tmp.path(),
            name: "x",
            inline: false,
            force: false,
        };
        make_node(&opts).unwrap();
        let err = make_node(&opts).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }
}
