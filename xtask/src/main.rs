//! Workspace task runner. Invoked via the `cargo xtask` alias.
//!
//! Currently a single subcommand: `gen-lua-types` regenerates the static
//! `types/crap.lua` from Rust source (`LuaAnnotation` derives,
//! `LuaAlias` derives, `#[lua_fn]` attributes). Run with `--check` in CI
//! to gate on drift between the on-disk file and what Rust would emit.
//!
//! Standard `cargo-xtask` pattern — keeps build-tool logic out of
//! `build.rs` (where it would re-run on every compile) and out of
//! shell scripts (where editor support and type safety go missing).
//!
//! ```bash
//! cargo xtask gen-lua-types          # regenerate types/crap.lua
//! cargo xtask gen-lua-types --check  # CI gate: fail if out of sync
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use similar::{ChangeTag, TextDiff};

/// `cargo xtask <subcommand>` entry point.
#[derive(Parser)]
#[command(name = "xtask", about = "crap-cms workspace task runner")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Regenerate the static `types/crap.lua` Lua type definition file
    /// from Rust source. With `--check`, exits non-zero (and prints a
    /// diff) when the on-disk file diverges from what would be generated.
    GenLuaTypes {
        /// Verify the on-disk file matches; do not write. Use in CI.
        #[arg(long)]
        check: bool,
    },
}

fn main() -> ExitCode {
    match Cli::parse().cmd {
        Cmd::GenLuaTypes { check } => match run_gen_lua_types(check) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e:#}");
                ExitCode::FAILURE
            }
        },
    }
}

/// Run the `gen-lua-types` subcommand.
///
/// In write mode (`check == false`): regenerates `types/crap.lua` from
/// Rust source and writes it to disk.
///
/// In check mode (`check == true`): compares the on-disk file against
/// the regenerated content; if they differ, prints a unified diff and
/// returns an error so CI fails.
fn run_gen_lua_types(check: bool) -> Result<()> {
    let path = lua_types_path()?;
    let generated = crap_cms::typegen::lua::render_static_file();

    if check {
        check_drift(&path, &generated)
    } else {
        std::fs::write(&path, &generated)
            .with_context(|| format!("failed to write {}", path.display()))?;
        println!("wrote {}", path.display());
        Ok(())
    }
}

/// Resolve the workspace-root-relative path to `types/crap.lua`.
///
/// `cargo xtask` runs with `CARGO_MANIFEST_DIR` set to the xtask crate's
/// own directory. The Lua type file is one level up.
fn lua_types_path() -> Result<PathBuf> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .context("CARGO_MANIFEST_DIR not set — run via `cargo xtask`")?;
    let mut path = PathBuf::from(manifest);
    path.pop(); // xtask/ → workspace root
    path.push("types");
    path.push("crap.lua");
    Ok(path)
}

/// Compare `generated` against the file at `path` and emit a unified
/// diff on stderr if they differ. Returns an error in that case so the
/// caller can exit non-zero.
fn check_drift(path: &std::path::Path, generated: &str) -> Result<()> {
    let existing = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    if existing == generated {
        println!("{} is up to date", path.display());
        return Ok(());
    }

    eprintln!("{} is out of sync with Rust source:", path.display());
    eprintln!();
    print_diff(&existing, generated);
    eprintln!();
    bail!("regenerate with `cargo xtask gen-lua-types` and commit the result");
}

/// Print a unified diff (3 lines of context per hunk) to stderr.
fn print_diff(existing: &str, generated: &str) {
    let diff = TextDiff::from_lines(existing, generated);
    for group in diff.grouped_ops(3) {
        for op in group {
            for change in diff.iter_changes(&op) {
                let sign = match change.tag() {
                    ChangeTag::Delete => "-",
                    ChangeTag::Insert => "+",
                    ChangeTag::Equal => " ",
                };
                eprint!("{sign}{change}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn check_drift_returns_ok_when_file_matches() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let content = "hello\nworld\n";
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();

        let result = check_drift(f.path(), content);
        assert!(
            result.is_ok(),
            "check_drift should succeed when file matches generated content"
        );
    }

    #[test]
    fn check_drift_returns_err_when_file_differs() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"on-disk\n").unwrap();
        f.flush().unwrap();

        let result = check_drift(f.path(), "regenerated\n");
        let err = result.expect_err("check_drift should fail when file diverges");
        let msg = format!("{err}");
        assert!(
            msg.contains("regenerate"),
            "error should advise running the regen command, got: {msg}"
        );
    }

    #[test]
    fn check_drift_returns_err_when_file_missing() {
        // Path under a tempdir we then drop — guarantees nonexistence.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.lua");

        let result = check_drift(&missing, "anything");
        assert!(
            result.is_err(),
            "check_drift should error when the target file doesn't exist"
        );
    }

    #[test]
    fn render_static_file_matches_repo_copy() {
        // Mirror of the main-crate `assembled_output_matches_on_disk`
        // test but at the xtask layer — proves that what `xtask
        // gen-lua-types --check` would compare against the on-disk
        // file is exactly what the typegen produces. Catches the
        // case where someone edits `types/crap.lua` by hand and
        // commits it without re-running the generator.
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
