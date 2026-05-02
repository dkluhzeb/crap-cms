//! `make node` — scaffold a custom richtext-node registration.
//!
//! Writes `<config_dir>/lua/richtext_nodes/<name>.lua` containing the
//! `crap.richtext.register_node(...)` call. Doesn't auto-modify
//! `init.lua` (we don't want a destructive AST rewrite); instead the
//! command prints the one-line `require()` to add manually.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};

use crate::{
    cli,
    scaffold::{to_title_case, validate_slug},
};

/// Options for `make_node`.
pub struct MakeNodeOptions<'a> {
    pub config_dir: &'a Path,
    pub name: &'a str,
    pub inline: bool,
    pub force: bool,
}

/// Scaffold the richtext-node Lua snippet.
pub fn make_node(opts: &MakeNodeOptions) -> Result<()> {
    validate_slug(opts.name)?;

    let dir = opts.config_dir.join("lua").join("richtext_nodes");
    fs::create_dir_all(&dir).context("Failed to create lua/richtext_nodes/ directory")?;

    let file_path = dir.join(format!("{}.lua", opts.name));
    if file_path.exists() && !opts.force {
        bail!(
            "File '{}' already exists — use --force to overwrite",
            file_path.display()
        );
    }

    let lua = render_node_lua(opts);
    fs::write(&file_path, &lua)
        .with_context(|| format!("Failed to write {}", file_path.display()))?;

    cli::success(&format!("Created {}", file_path.display()));
    cli::info(&format!(
        "Add this line to your init.lua to load the registration:\n\n  require(\"lua.richtext_nodes.{}\")",
        opts.name,
    ));

    Ok(())
}

fn render_node_lua(opts: &MakeNodeOptions) -> String {
    let label = to_title_case(opts.name);
    let inline_str = if opts.inline { "true" } else { "false" };
    format!(
        r#"--- Custom richtext node: {label}
---
--- Registered automatically when this file is `require()`d from
--- init.lua. Inserts as a {kind} node in the richtext block picker.
---
--- Attrs are typed via `crap.fields.*`. Allowed types: text, number,
--- textarea, select, radio, checkbox, date, email, json, code.
--- Render function returns the HTML the node serializes to; if
--- omitted, the node falls through to a `<crap-node>` passthrough for
--- client-side rendering.

crap.richtext.register_node("{name}", {{
  label = "{label}",
  inline = {inline_str},
  attrs = {{
    crap.fields.text({{
      name = "text",
      required = true,
      admin = {{ label = "Text", placeholder = "Display text" }},
    }}),
    -- Add more attrs here. Examples:
    -- crap.fields.text({{ name = "url", required = true,
    --                     admin = {{ label = "URL", placeholder = "https://..." }} }}),
    -- crap.fields.select({{
    --   name = "style",
    --   admin = {{ label = "Style" }},
    --   options = {{
    --     {{ label = "Primary", value = "primary" }},
    --     {{ label = "Secondary", value = "secondary" }},
    --   }},
    -- }}),
  }},
  searchable_attrs = {{ "text" }},
  render = function(attrs)
    -- Server-side HTML output. Escape user-controlled strings.
    return string.format(
      '<span class="{name}">%s</span>',
      attrs.text or ""
    )
  end,
}})
"#,
        name = opts.name,
        label = label,
        kind = if opts.inline { "inline" } else { "block-level" },
        inline_str = inline_str,
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
