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
