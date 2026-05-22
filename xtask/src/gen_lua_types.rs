//! `gen-lua-types` subcommand: regenerate / verify `types/crap.lua`.
//!
//! The static type-definition file is assembled from Rust derives
//! (`LuaAnnotation`, `LuaAlias`, `LuaTypeAlias`, `LuaFieldTypeViews`)
//! and `lua_table!`-generated `render_X_lua` fns via
//! [`crap_cms::typegen::lua::render_static_file`]. This subcommand
//! is the user-facing entry point: write in default mode, diff in
//! `--check` mode (CI gate).

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::drift::{check_drift, workspace_root};

/// Run the `gen-lua-types` subcommand.
pub(crate) fn run(check: bool) -> Result<()> {
    let path = lua_types_path()?;
    let generated = crap_cms::typegen::lua::render_static_file();

    if check {
        check_drift(&path, &generated, "cargo xtask gen-lua-types")
    } else {
        std::fs::write(&path, &generated)
            .with_context(|| format!("failed to write {}", path.display()))?;
        println!("wrote {}", path.display());
        Ok(())
    }
}

/// `<workspace-root>/types/crap.lua`.
fn lua_types_path() -> Result<PathBuf> {
    Ok(workspace_root()?.join("types").join("crap.lua"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_static_file_matches_repo_copy() {
        // Mirror of the main-crate `assembled_output_matches_on_disk`
        // test but at the xtask layer — proves that what
        // `cargo xtask gen-lua-types --check` would compare against
        // the on-disk file is exactly what the typegen produces.
        let generated = crap_cms::typegen::lua::render_static_file();
        let path = lua_types_path().expect("xtask manifest dir resolvable");
        let on_disk = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert_eq!(
            generated, on_disk,
            "types/crap.lua is out of sync with Rust source — run `cargo xtask gen-lua-types`"
        );
    }
}
