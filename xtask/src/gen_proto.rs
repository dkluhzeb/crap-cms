//! `gen-proto` subcommand: regenerate / verify the CRUD request message
//! bodies in `proto/content.proto` from the pinned wire spec
//! (`crap_cms::service::op::wire_proto`).
//!
//! Only the generated messages are touched — the rest of the proto file
//! (responses, auth, jobs, subscribe, the service block) is hand-written
//! and passes through unchanged. Mirrors the shape of
//! [`crate::gen_lua_types`]: write in default mode, diff in `--check` mode
//! (CI gate). An in-crate `#[test]` in `wire_proto.rs` asserts the same
//! sync, so plain `cargo test` catches drift too.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::drift::{check_drift, workspace_root};

/// Run the `gen-proto` subcommand.
pub(crate) fn run(check: bool) -> Result<()> {
    let path = proto_path()?;
    let src = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let generated = crap_cms::service::op::wire_proto::regenerate_proto(&src);

    if check {
        check_drift(&path, &generated, "cargo xtask gen-proto")
    } else {
        std::fs::write(&path, &generated)
            .with_context(|| format!("failed to write {}", path.display()))?;
        println!("wrote {}", path.display());
        Ok(())
    }
}

/// `<workspace-root>/proto/content.proto`.
fn proto_path() -> Result<PathBuf> {
    Ok(workspace_root()?.join("proto").join("content.proto"))
}
