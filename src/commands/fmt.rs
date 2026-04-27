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

pub fn run(paths: Vec<PathBuf>, check: bool, stdio: bool) -> Result<()> {
    if stdio {
        return run_stdio();
    }

    let targets = collect_targets(paths)?;
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
        fs::write(path, &formatted).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(true)
}

/// Resolve `paths` to a list of `.hbs` files. A directory expands
/// recursively; a file is taken as-is. An empty input defaults to
/// `templates/`.
fn collect_targets(paths: Vec<PathBuf>) -> Result<Vec<PathBuf>> {
    let inputs = if paths.is_empty() {
        vec![PathBuf::from("templates")]
    } else {
        paths
    };

    let mut out = Vec::new();
    for p in inputs {
        if p.is_file() {
            if !is_hbs(&p) {
                return Err(anyhow!("{} is not a .hbs file", p.display()));
            }
            out.push(p);
        } else if p.is_dir() {
            walk_dir(&p, &mut out)?;
        } else {
            return Err(anyhow!("path does not exist: {}", p.display()));
        }
    }
    out.sort();
    Ok(out)
}

fn is_hbs(p: &Path) -> bool {
    p.extension().is_some_and(|e| e == "hbs")
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            walk_dir(&path, out)?;
        } else if path.is_file() && is_hbs(&path) {
            out.push(path);
        }
    }
    Ok(())
}
