//! `crap-cms templates status` — walk the user's customized files,
//! parse the `crap-cms:source <version>` header from each, and report
//! the relationship to the running crap-cms version.
//!
//! Also exports [`customization_counts`] used by the main
//! `crap-cms status` command to surface customization state alongside
//! collections / migrations / jobs.

use std::{cmp::Ordering, fs, path::Path};

use anyhow::{Context as _, Result};
use semver::Version;

use crate::{cli, scaffold::source_header::parse_source_version};

use super::helpers::{CRATE_VERSION, lookup_embedded};

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

/// Run `crap-cms templates status` against the given config dir.
///
/// # Errors
///
/// Returns an error if scanning the overlay directory fails.
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
                ("⚠", format!("behind: extracted from {from}"))
            }
            Drift::Ahead { from } => {
                ahead += 1;
                ("↑", format!("ahead: extracted from {from}"))
            }
            Drift::UnknownVersion { raw } => {
                unknown += 1;
                ("?", format!("unparseable source header: {raw}"))
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
        "Summary: {current} current, {behind} behind, {ahead} ahead, {pristine} pristine, {unknown} unknown header, {no_header} no header, {orphaned} orphaned, {user_original} user-original"
    );

    if behind > 0 || orphaned > 0 {
        println!();
        cli::info("Run `crap-cms templates diff <PATH>` to compare a file against upstream.");
    }

    Ok(())
}

/// One-line counts of admin-UI customizations under the config dir.
/// Used by the main `crap-cms status` command to surface customization
/// state alongside collections / migrations / jobs.
#[derive(Debug, Clone, Default)]
pub struct CustomizationCounts {
    /// Files that shadow built-in defaults (any drift state except
    /// `UserOriginal`). Includes pristine + current + behind + ahead +
    /// orphaned + no-header + unknown-version.
    pub overrides: usize,
    /// User-original files with no upstream counterpart (custom pages,
    /// slot widgets, bespoke themes, custom Web Components, etc.).
    pub additions: usize,
    /// Files in a state the operator likely wants to act on:
    /// behind / ahead / orphaned / no-header / unknown-version.
    pub actionable: usize,
    /// Extracted-but-unedited files that could be deleted to fall back
    /// to upstream automatically.
    pub pristine: usize,
}

/// Walk the config dir's overlay roots and tally customizations.
/// Returns zeroed counts when neither `templates/` nor `static/`
/// exists (e.g. fresh install with only `init.lua`).
///
/// # Errors
///
/// Returns an error if scanning the overlay directory fails.
pub fn customization_counts(config_dir: &Path) -> Result<CustomizationCounts> {
    let entries = collect_overlay_entries(config_dir)?;
    let mut c = CustomizationCounts::default();
    for entry in &entries {
        match &entry.drift {
            Drift::UserOriginal => c.additions += 1,
            Drift::Pristine => {
                c.overrides += 1;
                c.pristine += 1;
            }
            Drift::Current => {
                c.overrides += 1;
            }
            Drift::Behind { .. }
            | Drift::Ahead { .. }
            | Drift::OrphanedUpstream
            | Drift::NoHeader
            | Drift::UnknownVersion { .. } => {
                c.overrides += 1;
                c.actionable += 1;
            }
        }
    }
    Ok(c)
}

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
        let rel_path = format!("{kind}/{sub_rel}");

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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn customization_counts_zero_for_fresh_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let counts = customization_counts(tmp.path()).unwrap();
        assert_eq!(counts.overrides, 0);
        assert_eq!(counts.additions, 0);
        assert_eq!(counts.actionable, 0);
        assert_eq!(counts.pristine, 0);
    }

    #[test]
    fn customization_counts_distinguishes_overrides_from_additions() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Override (extracted, with current-version header).
        let layout_dir = tmp.path().join("templates").join("layout");
        fs::create_dir_all(&layout_dir).unwrap();
        let crate_version = env!("CARGO_PKG_VERSION");
        // Use an embedded path that exists. Verified via `lookup_embedded`.
        let upstream = lookup_embedded("templates", "layout/base.hbs")
            .expect("layout/base.hbs must be embedded");
        let upstream_str = std::str::from_utf8(upstream).unwrap();
        let header = format!("{{{{!-- crap-cms:source {crate_version} --}}}}\n");
        let extracted = format!("{header}{upstream_str}");
        fs::write(layout_dir.join("base.hbs"), extracted).unwrap();

        // Addition (user-original — no embedded counterpart).
        let pages_dir = tmp.path().join("templates").join("pages");
        fs::create_dir_all(&pages_dir).unwrap();
        fs::write(pages_dir.join("custom_dashboard.hbs"), "{{!-- mine --}}").unwrap();

        let counts = customization_counts(tmp.path()).unwrap();
        assert_eq!(
            counts.overrides, 1,
            "extracted layout/base counts as override"
        );
        assert_eq!(counts.additions, 1, "user-original page counts as addition");
        assert_eq!(
            counts.actionable, 0,
            "current-version override needs no action"
        );
        assert_eq!(counts.pristine, 0);
    }

    #[test]
    fn customization_counts_flags_actionable_for_behind_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout_dir = tmp.path().join("templates").join("layout");
        fs::create_dir_all(&layout_dir).unwrap();
        // Stale source-version header → Drift::Behind, which is actionable.
        fs::write(
            layout_dir.join("base.hbs"),
            "{{!-- crap-cms:source 0.0.1-alpha.0 --}}\nfake old\n",
        )
        .unwrap();

        let counts = customization_counts(tmp.path()).unwrap();
        assert_eq!(counts.overrides, 1);
        assert_eq!(
            counts.actionable, 1,
            "behind file should be flagged actionable"
        );
    }

    #[test]
    fn customization_counts_flags_pristine_for_byte_equal_overrides() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout_dir = tmp.path().join("templates").join("layout");
        fs::create_dir_all(&layout_dir).unwrap();
        // Write the upstream content byte-for-byte (no header) — should
        // classify as Pristine.
        let upstream = lookup_embedded("templates", "layout/base.hbs")
            .expect("layout/base.hbs must be embedded");
        fs::write(layout_dir.join("base.hbs"), upstream).unwrap();

        let counts = customization_counts(tmp.path()).unwrap();
        assert_eq!(counts.overrides, 1);
        assert_eq!(counts.pristine, 1);
        assert_eq!(counts.actionable, 0);
    }
}
