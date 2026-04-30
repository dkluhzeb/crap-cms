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
use similar::{ChangeTag, TextDiff};

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
    /// No header found, but an upstream embedded version exists — the
    /// file is overriding a built-in default but was probably hand-written
    /// or had its header stripped.
    NoHeader,
    /// Header is present but the file no longer exists in the embedded
    /// upstream (deleted / renamed by a later release).
    OrphanedUpstream,
    /// File has no upstream embedded counterpart and no source header —
    /// it's user-authored content that was never part of the CMS
    /// (custom admin pages, custom slot files, plugin-shipped widgets,
    /// custom web components). Reported informationally; never a warning.
    UserOriginal,
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
    let mut user_original = 0usize;

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
                    "orphaned: extracted from upstream but no longer exists there".to_string(),
                )
            }
            Drift::UserOriginal => {
                user_original += 1;
                ("·", "user-original (no upstream counterpart)".to_string())
            }
        };

        println!("  {} {}  —  {}", icon, entry.rel_path, summary);
    }

    println!();
    println!(
        "Summary: {} current, {} behind, {} ahead, {} pristine, {} unknown header, {} no header, {} orphaned, {} user-original",
        current, behind, ahead, pristine, unknown, no_header, orphaned, user_original
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

    let user_text = String::from_utf8_lossy(&user_bytes);
    let header = parse_source_version(&user_text);
    let upstream = lookup_embedded(kind, sub_path);

    // Decision matrix:
    //
    //   has_upstream | has_header | classification
    //   ─────────────┼────────────┼──────────────────────────────
    //   yes          | (any)      | Pristine if byte-equal, else
    //                |            | classify by header version.
    //                |            | NoHeader if header missing.
    //   no           | yes        | OrphanedUpstream — file claims
    //                |            | to extend an upstream that's
    //                |            | gone.
    //   no           | no         | UserOriginal — never had an
    //                |            | upstream counterpart (custom
    //                |            | page, custom widget, etc.).

    match (upstream, &header) {
        (Some(upstream_bytes), _) if user_bytes == upstream_bytes => Ok(Drift::Pristine),
        (Some(_), None) => Ok(Drift::NoHeader),
        (Some(_), Some(raw)) => match (Version::parse(raw), Version::parse(CRATE_VERSION)) {
            (Ok(file_v), Ok(crate_v)) => Ok(match file_v.cmp(&crate_v) {
                Ordering::Equal => Drift::Current,
                Ordering::Less => Drift::Behind { from: raw.clone() },
                Ordering::Greater => Drift::Ahead { from: raw.clone() },
            }),
            _ => Ok(Drift::UnknownVersion { raw: raw.clone() }),
        },
        (None, Some(_)) => Ok(Drift::OrphanedUpstream),
        (None, None) => Ok(Drift::UserOriginal),
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

/// Render a unified-style line diff between `a` (upstream) and `b`
/// (user) to stdout. Uses the [`similar`] crate's Myers diff so adds /
/// deletes group correctly even when blocks of comments or new branches
/// have been inserted (the previous lockstep heuristic produced
/// unreadable noise on overlays that added more than a couple of lines).
fn print_unified_diff(label_a: &str, label_b: &str, a: &str, b: &str) {
    println!("--- {}", label_a);
    println!("+++ {}", label_b);

    let diff = TextDiff::from_lines(a, b);

    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };

        // `change.value()` includes the trailing newline if the source
        // line had one; keep formatting identical to the previous impl
        // by trimming that final `\n` and emitting our own newline via
        // println!.
        let line = change.value();
        let line = line.strip_suffix('\n').unwrap_or(line);
        println!("{sign}{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::print_unified_diff;

    /// Smoke: a clean insertion (block of new lines added between two
    /// matching anchors) renders as a contiguous run of `+` lines, not
    /// interleaved `-`/`+` noise.
    #[test]
    fn diff_groups_inserted_block() {
        // Capture stdout via a temp file is overkill — exercise the
        // backing similar crate's behaviour directly to assert the
        // grouping. (The println! body in `print_unified_diff` is a
        // thin formatter; the algorithmic correctness lives in
        // `TextDiff::from_lines` + `iter_all_changes`.)
        use similar::{ChangeTag, TextDiff};

        let upstream = "a\nb\nc\n";
        let user = "a\nNEW1\nNEW2\nb\nc\n";
        let diff = TextDiff::from_lines(upstream, user);

        let tags: Vec<_> = diff.iter_all_changes().map(|c| c.tag()).collect();
        // Expect: Equal(a), Insert(NEW1), Insert(NEW2), Equal(b), Equal(c)
        assert_eq!(
            tags,
            vec![
                ChangeTag::Equal,
                ChangeTag::Insert,
                ChangeTag::Insert,
                ChangeTag::Equal,
                ChangeTag::Equal,
            ]
        );
    }

    /// `print_unified_diff` shouldn't panic on empty inputs.
    #[test]
    fn diff_handles_empty_inputs() {
        print_unified_diff("a", "b", "", "");
        print_unified_diff("a", "b", "x\n", "");
        print_unified_diff("a", "b", "", "y\n");
    }
}
