//! `crap-cms fmt` — format Handlebars templates.
//!
//! Walks the given paths (defaulting to `templates/`), formats every
//! `.hbs` file via [`crate::fmt::format`], and either writes the
//! changes back or reports a non-zero exit when `--check` is set.
//!
//! `--stdio` reads from stdin and writes to stdout — used by editor
//! formatter integrations (conform.nvim).

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};

use crate::fmt::format;

/// Format Handlebars templates: format files in-place, or check them
/// without modification (`--check`), or read one template from stdin
/// and write formatted output to stdout (`--stdio`).
///
/// # Errors
///
/// Returns an error if a path can't be read or written, if `--check`
/// finds an unformatted file, or if stdio mode receives malformed input.
pub fn run(paths: Vec<PathBuf>, check: bool, stdio: bool, follow_symlinks: bool) -> Result<()> {
    if stdio {
        return run_stdio();
    }

    let targets = collect_targets(paths, follow_symlinks)?;
    if targets.is_empty() {
        bail!("no .hbs files found at the given paths");
    }

    let mut changed = Vec::new();
    let mut errors = Vec::new();

    for path in &targets {
        match format_file(path, check) {
            Ok(true) => changed.push(path.clone()),
            Ok(false) => {}
            Err(e) => errors.push((path.clone(), e)),
        }
    }

    for (path, err) in &errors {
        eprintln!("error: {}: {err:#}", path.display());
    }

    if check {
        if !changed.is_empty() {
            for p in &changed {
                println!("would reformat: {}", p.display());
            }
            bail!(
                "{} file(s) would be reformatted (run `crap-cms fmt` to apply)",
                changed.len()
            );
        }
        if !errors.is_empty() {
            bail!("{} file(s) failed to parse", errors.len());
        }
        println!("{} file(s) already formatted", targets.len());
        return Ok(());
    }

    for p in &changed {
        println!("formatted: {}", p.display());
    }
    if !errors.is_empty() {
        bail!("{} file(s) failed to format", errors.len());
    }
    Ok(())
}

fn run_stdio() -> Result<()> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .context("reading stdin")?;
    let formatted = format(&input)?;
    io::stdout()
        .write_all(formatted.as_bytes())
        .context("writing stdout")?;
    Ok(())
}

/// Format a single file. Returns `Ok(true)` if the file's contents
/// would change; `Ok(false)` if already-formatted; `Err` on parse
/// failure.
fn format_file(path: &Path, check: bool) -> Result<bool> {
    let original =
        fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let formatted = format(&original).with_context(|| format!("formatting {}", path.display()))?;
    if formatted == original {
        return Ok(false);
    }
    if !check {
        write_atomic(path, &formatted).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(true)
}

/// Write `contents` to `path` atomically: write a sibling temp file, then
/// rename it over the target (an atomic replace on the same filesystem), so
/// a crash mid-write — this runs in pre-commit hooks — can never leave a
/// truncated file.
fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("invalid path: {}", path.display()))?
        .to_string_lossy();
    let tmp = dir.join(format!(".{name}.fmt{}.tmp", std::process::id()));

    fs::write(&tmp, contents).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("replacing {}", path.display()));
    }
    Ok(())
}

/// Resolve `paths` to a list of `.hbs` files. A directory expands
/// recursively; a file is taken as-is. An empty input defaults to
/// `templates/`. Symlinks are skipped unless `follow_symlinks` is set.
fn collect_targets(paths: Vec<PathBuf>, follow_symlinks: bool) -> Result<Vec<PathBuf>> {
    let inputs = if paths.is_empty() {
        vec![PathBuf::from("templates")]
    } else {
        paths
    };

    let mut out = Vec::new();
    for p in inputs {
        if !follow_symlinks && is_symlink(&p) {
            return Err(anyhow!(
                "{} is a symlink; pass --follow-symlinks to format it",
                p.display()
            ));
        }
        if p.is_file() {
            if !is_hbs(&p) {
                return Err(anyhow!("{} is not a .hbs file", p.display()));
            }
            out.push(p);
        } else if p.is_dir() {
            walk_dir(&p, &mut out, follow_symlinks)?;
        } else {
            return Err(anyhow!("path does not exist: {}", p.display()));
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn is_hbs(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "hbs")
}

fn is_symlink(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>, follow_symlinks: bool) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();

        // Skip symlinked entries by default so a link can't pull the
        // formatter outside the tree (a symlinked dir) or write through to
        // a target elsewhere (a symlinked `.hbs`).
        if !follow_symlinks && is_symlink(&path) {
            continue;
        }
        if path.is_dir() {
            walk_dir(&path, out, follow_symlinks)?;
        } else if path.is_file() && is_hbs(&path) {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_hbs_matches_only_the_lowercase_hbs_extension() {
        assert!(is_hbs(Path::new("templates/base.hbs")));
        assert!(!is_hbs(Path::new("styles.css")));
        assert!(!is_hbs(Path::new("README"))); // no extension
        assert!(!is_hbs(Path::new("base.HBS"))); // case-sensitive
        assert!(!is_hbs(Path::new("base.hbs.bak")));
    }

    #[test]
    fn collect_targets_dedups_a_dir_and_a_file_inside_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.hbs"), "x").unwrap();
        fs::write(root.join("b.hbs"), "x").unwrap();

        let got = collect_targets(vec![root.to_path_buf(), root.join("a.hbs")], false).unwrap();

        let a_count = got.iter().filter(|p| p.ends_with("a.hbs")).count();
        assert_eq!(a_count, 1, "a.hbs must appear once: {got:?}");
    }

    #[cfg(unix)]
    #[test]
    fn collect_targets_skips_symlinks_unless_following() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("real.hbs"), "x").unwrap();
        symlink(root.join("real.hbs"), root.join("link.hbs")).unwrap();

        let default = collect_targets(vec![root.to_path_buf()], false).unwrap();
        assert!(default.iter().any(|p| p.ends_with("real.hbs")));
        assert!(
            !default.iter().any(|p| p.ends_with("link.hbs")),
            "symlink skipped by default: {default:?}"
        );

        let followed = collect_targets(vec![root.to_path_buf()], true).unwrap();
        assert!(
            followed.iter().any(|p| p.ends_with("link.hbs")),
            "symlink followed with the flag: {followed:?}"
        );
    }

    #[test]
    fn write_atomic_writes_expected_bytes_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.hbs");
        write_atomic(&path, "hello\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello\n");
        // No leftover temp files in the directory.
        let stray = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains(".tmp"));
        assert!(!stray, "atomic write left a temp file behind");
    }
}
