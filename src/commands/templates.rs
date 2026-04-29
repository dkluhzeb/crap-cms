//! `crap-cms templates` — manage and inspect the user's customization
//! layer (the files in `<config_dir>/{templates,static}/` that override
//! the compiled-in defaults).
//!
//! - [`list`] / [`extract`] — bootstrap helpers for getting starter files
//!   from the embedded defaults.
//! - [`status`] walks the customized files, parses the `crap-cms:source
//!   <version>` header from each, and reports the relationship to the
//!   running crap-cms version.
//! - [`diff`] takes one customized file and shows a unified-style diff
//!   between the user's copy and the embedded default.

use std::{cmp::Ordering, fs, path::Path};

use anyhow::{Context as _, Result, bail};
use include_dir::Dir;
use semver::Version;

use crate::{
    cli, scaffold,
    scaffold::{
        source_header::parse_source_version,
        templates::{EMBEDDED_STATIC, EMBEDDED_TEMPLATES},
    },
};

/// Handle the `templates list` subcommand (no config needed — lists
/// embedded defaults shipped with the binary).
pub fn list(r#type: Option<String>, verbose: bool) -> Result<()> {
    scaffold::templates_list(r#type.as_deref(), verbose)
}

/// Handle the `templates extract` subcommand (writes embedded defaults
/// into the config dir, with source-version headers).
pub fn extract(
    config_dir: &Path,
    paths: &[String],
    all: bool,
    r#type: Option<String>,
    force: bool,
) -> Result<()> {
    scaffold::templates_extract(config_dir, paths, all, r#type.as_deref(), force)
}

/// Current crate version — what an overlay file's source-version header
/// is compared against.
const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Per-file drift classification.
enum Drift {
    /// File matches the upstream default byte-for-byte (rare — would mean
    /// the user extracted but never customized).
    Pristine,
    /// Header version equals the current crate version. Whether the
    /// content matches is a separate question (`overlay diff` answers it).
    Current,
    /// Header version is older than the current crate version.
    Behind { from: String },
    /// Header version is newer than the current crate version (downgrade
    /// scenario or pre-release oddity).
    Ahead { from: String },
    /// Header version is present but unparseable as semver.
    UnknownVersion { raw: String },
    /// No header found — probably hand-written or comment-stripped.
    NoHeader,
    /// Header is present but the file no longer exists in the embedded
    /// upstream (deleted / renamed by a later release).
    OrphanedUpstream,
}

struct OverlayEntry {
    /// Relative path inside the config dir, including the `templates/` or
    /// `static/` prefix (e.g. `templates/layout/base.hbs`).
    rel_path: String,
    drift: Drift,
}

/// Run `crap-cms overlay status` against the given config dir.
pub fn status(config_dir: &Path) -> Result<()> {
    let entries = collect_overlay_entries(config_dir)?;

    if entries.is_empty() {
        cli::info(&format!(
            "No customizations in {} — nothing to report.",
            config_dir.display()
        ));
        cli::info("Extract a default to start customizing:");
        cli::info("  crap-cms templates extract <PATH>");
        return Ok(());
    }

    let mut current = 0usize;
    let mut pristine = 0usize;
    let mut behind = 0usize;
    let mut ahead = 0usize;
    let mut unknown = 0usize;
    let mut no_header = 0usize;
    let mut orphaned = 0usize;

    println!(
        "Templates customization status (config dir: {}, running version: {})",
        config_dir.display(),
        CRATE_VERSION
    );
    println!();

    for entry in &entries {
        let (icon, summary) = match &entry.drift {
            Drift::Pristine => {
                pristine += 1;
                ("=", "pristine (matches upstream)".to_string())
            }
            Drift::Current => {
                current += 1;
                ("✓", "current".to_string())
            }
            Drift::Behind { from } => {
                behind += 1;
                ("⚠", format!("behind: extracted from {}", from))
            }
            Drift::Ahead { from } => {
                ahead += 1;
                ("↑", format!("ahead: extracted from {}", from))
            }
            Drift::UnknownVersion { raw } => {
                unknown += 1;
                ("?", format!("unparseable source header: {}", raw))
            }
            Drift::NoHeader => {
                no_header += 1;
                (
                    "?",
                    "no source header (hand-written or stripped)".to_string(),
                )
            }
            Drift::OrphanedUpstream => {
                orphaned += 1;
                (
                    "✗",
                    "orphaned: no longer exists in upstream embedded files".to_string(),
                )
            }
        };

        println!("  {} {}  —  {}", icon, entry.rel_path, summary);
    }

    println!();
    println!(
        "Summary: {} current, {} behind, {} ahead, {} pristine, {} unknown header, {} no header, {} orphaned",
        current, behind, ahead, pristine, unknown, no_header, orphaned
    );

    if behind > 0 || orphaned > 0 {
        println!();
        cli::info("Run `crap-cms templates diff <PATH>` to compare a file against upstream.");
    }

    Ok(())
}

/// Run `crap-cms overlay diff` for a single overlay path. The path is
/// relative to the config dir (e.g. `templates/layout/base.hbs` or
/// `static/styles.css`).
pub fn diff(config_dir: &Path, rel_path: &str) -> Result<()> {
    let abs = config_dir.join(rel_path);
    if !abs.exists() {
        bail!(
            "Overlay file not found: {} (relative to {})",
            rel_path,
            config_dir.display()
        );
    }

    let user =
        fs::read_to_string(&abs).with_context(|| format!("read overlay file {}", abs.display()))?;

    let Some((kind, sub_path)) = split_kind(rel_path) else {
        bail!(
            "Overlay path must start with `templates/` or `static/`, got: {}",
            rel_path
        );
    };

    let embedded = lookup_embedded(kind, sub_path).with_context(|| {
        format!(
            "no embedded upstream for {}/{} — has it been removed in this version?",
            kind, sub_path
        )
    })?;

    let upstream = String::from_utf8_lossy(embedded);

    print_unified_diff(
        &format!("upstream/{}", rel_path),
        &abs.display().to_string(),
        &upstream,
        &user,
    );

    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────

fn collect_overlay_entries(config_dir: &Path) -> Result<Vec<OverlayEntry>> {
    let mut entries = Vec::new();
    for kind in ["templates", "static"] {
        let root = config_dir.join(kind);
        if !root.exists() {
            continue;
        }

        walk_overlay_dir(&root, &root, kind, &mut entries)?;
    }

    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(entries)
}

fn walk_overlay_dir(
    root: &Path,
    cur: &Path,
    kind: &str,
    out: &mut Vec<OverlayEntry>,
) -> Result<()> {
    for entry in fs::read_dir(cur).with_context(|| format!("read directory {}", cur.display()))? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            walk_overlay_dir(root, &path, kind, out)?;
            continue;
        }

        let sub_rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .to_string();
        let rel_path = format!("{}/{}", kind, sub_rel);

        let drift = classify_file(&path, kind, &sub_rel)?;
        out.push(OverlayEntry { rel_path, drift });
    }
    Ok(())
}

fn classify_file(abs: &Path, kind: &str, sub_path: &str) -> Result<Drift> {
    let user_bytes =
        fs::read(abs).with_context(|| format!("read overlay file {}", abs.display()))?;

    // Pristine check: byte-equal to upstream (header included or not).
    if let Some(upstream) = lookup_embedded(kind, sub_path) {
        if user_bytes == upstream {
            return Ok(Drift::Pristine);
        }
    } else {
        return Ok(Drift::OrphanedUpstream);
    }

    let user_text = String::from_utf8_lossy(&user_bytes);
    let header = parse_source_version(&user_text);
    let Some(raw) = header else {
        return Ok(Drift::NoHeader);
    };

    match (Version::parse(&raw), Version::parse(CRATE_VERSION)) {
        (Ok(file_v), Ok(crate_v)) => Ok(match file_v.cmp(&crate_v) {
            Ordering::Equal => Drift::Current,
            Ordering::Less => Drift::Behind { from: raw },
            Ordering::Greater => Drift::Ahead { from: raw },
        }),
        _ => Ok(Drift::UnknownVersion { raw }),
    }
}

fn split_kind(rel_path: &str) -> Option<(&'static str, &str)> {
    if let Some(rest) = rel_path.strip_prefix("templates/") {
        Some(("templates", rest))
    } else if let Some(rest) = rel_path.strip_prefix("static/") {
        Some(("static", rest))
    } else {
        None
    }
}

fn lookup_embedded(kind: &str, sub_path: &str) -> Option<&'static [u8]> {
    let dir: &'static Dir = match kind {
        "templates" => &EMBEDDED_TEMPLATES,
        "static" => &EMBEDDED_STATIC,
        _ => return None,
    };

    dir.get_file(sub_path).map(|f| f.contents())
}

/// Tiny line-diff printer — no external diff dependency. Shows context
/// lines around changed regions; identical lines are summarized.
fn print_unified_diff(label_a: &str, label_b: &str, a: &str, b: &str) {
    println!("--- {}", label_a);
    println!("+++ {}", label_b);

    let a_lines: Vec<&str> = a.lines().collect();
    let b_lines: Vec<&str> = b.lines().collect();

    // Use the standard LCS-based diff via a simple Myers-like sweep —
    // keep it minimal: walk both sides emitting `-` / `+` for differing
    // regions, ` ` (space) for matched lines.
    let mut i = 0usize;
    let mut j = 0usize;

    while i < a_lines.len() || j < b_lines.len() {
        match (a_lines.get(i), b_lines.get(j)) {
            (Some(la), Some(lb)) if la == lb => {
                println!(" {}", la);
                i += 1;
                j += 1;
            }
            (Some(la), Some(lb)) => {
                // Heuristic: look ahead a few lines on each side to find
                // the next match — emit minus/plus for the difference
                // region.
                let next_match_b = b_lines[j..]
                    .iter()
                    .take(20)
                    .position(|l| Some(l) == Some(la));
                let next_match_a = a_lines[i..]
                    .iter()
                    .take(20)
                    .position(|l| Some(l) == Some(lb));

                match (next_match_a, next_match_b) {
                    (Some(skip_a), _) => {
                        for k in 0..skip_a {
                            println!("-{}", a_lines[i + k]);
                        }
                        i += skip_a;
                    }
                    (None, Some(skip_b)) => {
                        for k in 0..skip_b {
                            println!("+{}", b_lines[j + k]);
                        }
                        j += skip_b;
                    }
                    (None, None) => {
                        println!("-{}", la);
                        println!("+{}", lb);
                        i += 1;
                        j += 1;
                    }
                }
            }
            (Some(la), None) => {
                println!("-{}", la);
                i += 1;
            }
            (None, Some(lb)) => {
                println!("+{}", lb);
                j += 1;
            }
            (None, None) => break,
        }
    }
}
