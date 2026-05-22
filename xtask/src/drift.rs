//! Cross-subcommand helpers: workspace path resolution + drift check
//! with unified-diff output.
//!
//! Both `gen-lua-types` and `gen-template-doc` follow the same shape:
//! render Rust source to a String, then either write it to disk or
//! compare against the committed copy. The shared bits live here so
//! each subcommand stays focused on "what to render where."

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use similar::{ChangeTag, TextDiff};

/// Workspace root directory. `cargo xtask` invokes the binary with
/// `CARGO_MANIFEST_DIR` set to the xtask crate's own directory, so
/// the workspace root is one level up.
pub(crate) fn workspace_root() -> Result<PathBuf> {
    let manifest = std::env::var("CARGO_MANIFEST_DIR")
        .context("CARGO_MANIFEST_DIR not set — run via `cargo xtask`")?;
    let mut path = PathBuf::from(manifest);
    path.pop();
    Ok(path)
}

/// Compare `generated` against the file at `path` and emit a unified
/// diff on stderr if they differ. Returns an error in that case so the
/// caller can exit non-zero. `regen_cmd` is the user-facing command
/// recommended in the error message (e.g. `cargo xtask gen-lua-types`).
pub(crate) fn check_drift(path: &Path, generated: &str, regen_cmd: &str) -> Result<()> {
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
    bail!("regenerate with `{regen_cmd}` and commit the result");
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
    fn returns_ok_when_file_matches() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        let content = "hello\nworld\n";
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();

        let result = check_drift(f.path(), content, "regen-foo");
        assert!(
            result.is_ok(),
            "check_drift should succeed when file matches generated content"
        );
    }

    #[test]
    fn returns_err_when_file_differs() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"on-disk\n").unwrap();
        f.flush().unwrap();

        let result = check_drift(f.path(), "regenerated\n", "regen-foo");
        let err = result.expect_err("check_drift should fail when file diverges");
        let msg = format!("{err}");
        assert!(
            msg.contains("regen-foo"),
            "error should reference the regen command name, got: {msg}"
        );
    }

    #[test]
    fn returns_err_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");

        let result = check_drift(&missing, "anything", "regen-foo");
        assert!(
            result.is_err(),
            "check_drift should error when the target file doesn't exist"
        );
    }
}
