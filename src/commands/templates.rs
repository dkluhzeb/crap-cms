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

/// Run `crap-cms templates layout` — report old-layout files in the
/// config dir and recommend `git mv` commands.
///
/// **Read-only.** This command describes what should move; it never
/// moves files, and never rewrites file contents. The user runs the
/// recommended commands themselves, then verifies (because the tool
/// can't safely rewrite imports inside moved JS files, partial-by-path
/// references inside HBS files, or `@import url(...)` references
/// inside CSS files).
///
/// Output sections:
///
/// 1. **Old layout detected** — files whose path matches the old
///    layout but should move; printed as `OLD → NEW`.
/// 2. **Recommended migration** — copy-pasteable `mkdir -p` and
///    `git mv` commands.
/// 3. **Things the tool can't safely rewrite** — bullet list of
///    after-move verifications the user must perform.
/// 4. **Files the tool can't categorize** — paths under the overlay
///    roots that don't match either the old or new layout. These are
///    user-original files (custom widgets, bespoke themes); listed
///    informationally so the user knows the tool didn't lose them.
pub fn layout(config_dir: &Path) -> Result<()> {
    let entries = collect_layout_entries(config_dir)?;

    let old_layout: Vec<_> = entries
        .iter()
        .filter_map(|e| match &e.kind {
            LayoutKind::OldLayout { new_path } => Some((e.rel_path.as_str(), new_path.as_str())),
            _ => None,
        })
        .collect();

    let unknowns: Vec<_> = entries
        .iter()
        .filter(|e| matches!(e.kind, LayoutKind::Unknown))
        .map(|e| e.rel_path.as_str())
        .collect();

    if old_layout.is_empty() && unknowns.is_empty() {
        cli::info(&format!(
            "Config dir {} is already on the current layout — nothing to migrate.",
            config_dir.display()
        ));
        return Ok(());
    }

    if !old_layout.is_empty() {
        println!("Old layout detected ({} files):", old_layout.len());
        for (old, new) in &old_layout {
            println!("  {} → {}", old, new);
        }
        println!();

        // Group entries by destination. Multiple old files mapping to
        // the same new file (e.g. `lists.css` + `list-toolbar.css` →
        // `parts/lists.css`) are a *merge* — `git mv` won't work,
        // they need to be concatenated.
        let mut by_new: std::collections::BTreeMap<&str, Vec<&str>> =
            std::collections::BTreeMap::new();
        for (old, new) in &old_layout {
            by_new.entry(new).or_default().push(old);
        }

        // Recipe section — group new-path parent directories so a
        // single `mkdir -p` covers each prefix.
        let mut new_dirs: Vec<&str> = old_layout
            .iter()
            .filter_map(|(_, new)| Path::new(new).parent().and_then(|p| p.to_str()))
            .collect();
        new_dirs.sort_unstable();
        new_dirs.dedup();

        println!("Recommended migration (run from {}):", config_dir.display());
        if !new_dirs.is_empty() {
            println!("  mkdir -p {}", new_dirs.join(" "));
        }
        for (new, olds) in &by_new {
            if olds.len() == 1 {
                println!("  git mv {} {}", olds[0], new);
            } else {
                // Merge case: multiple old files into one new file.
                // Concatenate, then delete the originals.
                println!("  # MERGE — {} old files into {}", olds.len(), new);
                println!("  cat {} > {}", olds.join(" "), new);
                println!("  git rm {}", olds.join(" "));
                println!("  git add {}", new);
            }
        }
        println!();

        println!("After moving, verify these things the tool can't safely rewrite:");
        println!("  • `import` paths inside moved JS files (relative paths may break).");
        println!("  • `{{{{> \"path/to/partial\"}}}}` references in HBS (name lookups are safe).");
        println!("  • `@import url(...)` references in moved CSS files.");
        println!("  • `<link>` / `<script>` URLs in any layout HBS files you've overridden.");
        println!();
        println!("Then run `crap-cms templates status` to confirm drift visibility re-attaches.");
    }

    if !unknowns.is_empty() {
        if !old_layout.is_empty() {
            println!();
        }
        println!(
            "Files the tool can't categorize ({} — likely user-original, leave as-is):",
            unknowns.len()
        );
        for path in &unknowns {
            println!("  {}", path);
        }
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

// ── Layout report helpers ──────────────────────────────────────────

/// Per-file classification for `templates layout`.
enum LayoutKind {
    /// File matches a known old-layout path; should move to `new_path`.
    OldLayout { new_path: String },
    /// File is already on the current layout — nothing to do.
    OnCurrentLayout,
    /// File is under an overlay root but matches neither layout. Likely
    /// user-original (custom widget, bespoke theme); listed
    /// informationally.
    Unknown,
}

struct LayoutEntry {
    rel_path: String,
    kind: LayoutKind,
}

/// Walk the config dir's overlay roots and classify every file.
fn collect_layout_entries(config_dir: &Path) -> Result<Vec<LayoutEntry>> {
    let mut entries = Vec::new();

    for kind in ["templates", "static"] {
        let root = config_dir.join(kind);
        if !root.exists() {
            continue;
        }
        walk_layout_dir(&root, &root, kind, &mut entries)?;
    }

    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(entries)
}

fn walk_layout_dir(root: &Path, cur: &Path, kind: &str, out: &mut Vec<LayoutEntry>) -> Result<()> {
    for entry in fs::read_dir(cur).with_context(|| format!("read directory {}", cur.display()))? {
        let entry = entry?;
        let path = entry.path();

        // Skip the sidecar metadata directory (Phase G) — it lives at
        // `.crap-overlay/` and isn't part of the overlay surface.
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|name| name.starts_with('.'))
        {
            continue;
        }

        if path.is_dir() {
            walk_layout_dir(root, &path, kind, out)?;
            continue;
        }

        let sub_rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let rel_path = format!("{}/{}", kind, sub_rel);

        let layout_kind = classify_layout_path(kind, &sub_rel);
        out.push(LayoutEntry {
            rel_path,
            kind: layout_kind,
        });
    }
    Ok(())
}

/// Classify a single file path against the layout move maps.
///
/// Returns [`LayoutKind::OldLayout`] when the path matches a known
/// old-layout entry, [`LayoutKind::OnCurrentLayout`] when it's already
/// at a known current-layout path, and [`LayoutKind::Unknown`]
/// otherwise (user-original).
fn classify_layout_path(kind: &str, sub_path: &str) -> LayoutKind {
    if let Some(new_sub) = lookup_layout_move(kind, sub_path) {
        return LayoutKind::OldLayout {
            new_path: format!("{}/{}", kind, new_sub),
        };
    }

    // Already on the current layout? It is if the sub_path resolves in
    // the embedded dir, OR if it's inside a known current-layout
    // prefix (so user-original files at a current path don't get
    // flagged as "unknown" just because they're not embedded).
    if lookup_embedded(kind, sub_path).is_some() || matches_current_layout_prefix(kind, sub_path) {
        return LayoutKind::OnCurrentLayout;
    }

    LayoutKind::Unknown
}

/// Match a sub-path against the old-layout → new-layout move table.
///
/// Per-file exact matches take priority over prefix matches so a file
/// like `static/lists.css` (merged into `lists.css` rather than copied
/// 1:1) classifies correctly. Returns the new sub-path on hit.
///
/// **This table is the single source of truth for the layout move.**
/// Phases B–E check entries off as they land; the layout report and
/// the static-handler/template-registry alias tables both derive from
/// this same view.
fn lookup_layout_move(kind: &str, sub_path: &str) -> Option<String> {
    // Layer 1: exact-path renames (one-off moves).
    for (k, old, new) in EXACT_LAYOUT_MOVES {
        if *k == kind && *old == sub_path {
            return Some((*new).to_string());
        }
    }

    // Layer 2: directory prefix moves. e.g. `templates/auth/login.hbs`
    // → `templates/pages/auth/login.hbs` is a prefix rule
    // `auth/` → `pages/auth/`.
    for (k, old_prefix, new_prefix) in PREFIX_LAYOUT_MOVES {
        if *k == kind
            && let Some(rest) = sub_path.strip_prefix(old_prefix)
        {
            return Some(format!("{}{}", new_prefix, rest));
        }
    }

    None
}

/// Return whether `sub_path` is inside one of the *current-layout*
/// directory prefixes — used to distinguish "user-original at a known
/// path" from "user-original somewhere weird."
fn matches_current_layout_prefix(kind: &str, sub_path: &str) -> bool {
    for (k, prefix) in CURRENT_LAYOUT_PREFIXES {
        if *k == kind && sub_path.starts_with(prefix) {
            return true;
        }
    }
    false
}

/// Exact-path moves: `(kind, old_sub_path, new_sub_path)`.
///
/// Populated by Phases B–E as files actually move. Empty in this
/// commit — the layout command runs (and the test suite passes)
/// against an empty table; entries land alongside the file moves
/// themselves.
const EXACT_LAYOUT_MOVES: &[(&str, &str, &str)] = &[
    // Phase B (CSS) — every old root-level CSS file mapped to its new
    // bucket. `lists.css` + `list-toolbar.css` are both reported as
    // moving to the same merged file `parts/lists.css`.
    ("static", "styles.css", "styles/main.css"),
    ("static", "normalize.css", "styles/base/normalize.css"),
    ("static", "fonts.css", "styles/base/fonts.css"),
    ("static", "badges.css", "styles/parts/badges.css"),
    ("static", "breadcrumb.css", "styles/parts/breadcrumb.css"),
    ("static", "buttons.css", "styles/parts/buttons.css"),
    ("static", "cards.css", "styles/parts/cards.css"),
    ("static", "forms.css", "styles/parts/forms.css"),
    ("static", "tables.css", "styles/parts/tables.css"),
    ("static", "layout.css", "styles/layout/layout.css"),
    (
        "static",
        "edit-sidebar.css",
        "styles/layout/edit-sidebar.css",
    ),
    ("static", "themes.css", "styles/themes/default.css"),
    ("static", "lists.css", "styles/parts/lists.css"),
    ("static", "list-toolbar.css", "styles/parts/lists.css"),
    //
    // Phase C — vendored third-party bundles + icons.
    ("static", "htmx.js", "vendor/htmx.js"),
    ("static", "codemirror.js", "vendor/codemirror.js"),
    ("static", "prosemirror.js", "vendor/prosemirror.js"),
    ("static", "favicon.svg", "icons/favicon.svg"),
    ("static", "crap-cms.svg", "icons/crap-cms.svg"),
    //
    // Phase D — plumbing JS modules moved into `_internal/`. Public
    // components stayed flat, so most JS override paths are unchanged.
    ("static", "components/css.js", "components/_internal/css.js"),
    (
        "static",
        "components/global.js",
        "components/_internal/global.js",
    ),
    (
        "static",
        "components/groups.js",
        "components/_internal/groups.js",
    ),
    ("static", "components/h.js", "components/_internal/h.js"),
    (
        "static",
        "components/i18n.js",
        "components/_internal/i18n.js",
    ),
    (
        "static",
        "components/picker-base.js",
        "components/_internal/picker-base.js",
    ),
    (
        "static",
        "components/util/cookies.js",
        "components/_internal/util/cookies.js",
    ),
    (
        "static",
        "components/util/discover.js",
        "components/_internal/util/discover.js",
    ),
    (
        "static",
        "components/util/htmx.js",
        "components/_internal/util/htmx.js",
    ),
    (
        "static",
        "components/util/index.js",
        "components/_internal/util/index.js",
    ),
    (
        "static",
        "components/util/json.js",
        "components/_internal/util/json.js",
    ),
    (
        "static",
        "components/util/toast.js",
        "components/_internal/util/toast.js",
    ),
];

/// Directory prefix moves: `(kind, old_prefix, new_prefix)`. Both
/// prefixes include their trailing `/`.
///
/// Phase D (JS components by role) and Phase E (templates by page
/// family) populate this table.
const PREFIX_LAYOUT_MOVES: &[(&str, &str, &str)] = &[
    // Phase E (templates/) — populated when page templates move:
    //   ("templates", "auth/", "pages/auth/"),
    //   ("templates", "collections/", "pages/collections/"),
    //   ("templates", "dashboard/", "pages/dashboard/"),
    //   ("templates", "errors/", "pages/errors/"),
    //   ("templates", "globals/", "pages/globals/"),
    //
    // Phase D (JS by role) — populated per the JS map. Note: this is
    // file-level, not a folder-level move per JS file's role bucket;
    // EXACT_LAYOUT_MOVES will carry those instead.
];

/// Current-layout directory prefixes — used to recognize that a
/// user-original file at e.g. `templates/pages/custom/foo.hbs` is on
/// the current layout (just user-original), not "unknown."
const CURRENT_LAYOUT_PREFIXES: &[(&str, &str)] = &[
    // Templates: each top-level current-layout dir. Includes both the
    // shipped page-family folders (auth/, collections/, dashboard/,
    // errors/, globals/) — Phase E was a research-driven no-op, so
    // these stay flat instead of moving under `pages/` — and the
    // reserved-but-empty slot for filesystem-routed custom pages
    // (`pages/`, used by `/admin/p/<slug>` rendering).
    ("templates", "layout/"),
    ("templates", "pages/"),
    ("templates", "partials/"),
    ("templates", "fields/"),
    ("templates", "auth/"),
    ("templates", "collections/"),
    ("templates", "dashboard/"),
    ("templates", "errors/"),
    ("templates", "globals/"),
    ("templates", "slots/"),
    ("templates", "email/"),
    // Static: each top-level current-layout dir.
    ("static", "components/"),
    ("static", "styles/"),
    ("static", "vendor/"),
    ("static", "fonts/"),
    ("static", "icons/"),
];

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
    use super::{customization_counts, lookup_embedded, print_unified_diff};

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

    // ── Layout report tests ────────────────────────────────────────

    use std::fs;

    use super::{
        EXACT_LAYOUT_MOVES, LayoutKind, PREFIX_LAYOUT_MOVES, classify_layout_path,
        collect_layout_entries, layout, lookup_layout_move, matches_current_layout_prefix,
    };

    /// `lookup_layout_move` returns `Some(new_sub)` for known
    /// old-layout paths and `None` for paths that aren't on the move
    /// list. Each Phase B–E populates entries; this test exercises
    /// whichever rules are currently live.
    #[test]
    fn lookup_returns_some_for_known_old_paths() {
        // Phase B: the old root-level CSS files all map into static/styles/.
        assert_eq!(
            lookup_layout_move("static", "styles.css").as_deref(),
            Some("styles/main.css"),
        );
        assert_eq!(
            lookup_layout_move("static", "lists.css").as_deref(),
            Some("styles/parts/lists.css"),
        );
        assert_eq!(
            lookup_layout_move("static", "list-toolbar.css").as_deref(),
            Some("styles/parts/lists.css"),
        );
        assert_eq!(
            lookup_layout_move("static", "themes.css").as_deref(),
            Some("styles/themes/default.css"),
        );

        // Paths already on the new layout aren't moved.
        assert!(lookup_layout_move("static", "styles/main.css").is_none());
        assert!(lookup_layout_move("static", "components/toast.js").is_none());

        // Smoke the table types are addressable — ensures Phase D/E
        // populate the right places.
        let _: &[(&str, &str, &str)] = EXACT_LAYOUT_MOVES;
        let _: &[(&str, &str, &str)] = PREFIX_LAYOUT_MOVES;
    }

    /// `classify_layout_path` distinguishes current-layout, old-layout,
    /// and unknown buckets correctly.
    #[test]
    fn classify_layout_path_buckets() {
        // A real embedded template is on the current layout.
        assert!(matches!(
            classify_layout_path("templates", "auth/login.hbs"),
            LayoutKind::OnCurrentLayout
        ));

        // An old-layout CSS path classifies as OldLayout with the new
        // path attached.
        match classify_layout_path("static", "styles.css") {
            LayoutKind::OldLayout { new_path } => {
                assert_eq!(new_path, "static/styles/main.css")
            }
            other => panic!(
                "expected OldLayout for static/styles.css, got {:?}",
                std::mem::discriminant(&other)
            ),
        }

        // A user-original path that's not embedded, not on the move
        // list, and not matched by a current-layout prefix is Unknown.
        assert!(matches!(
            classify_layout_path("static", "my-bespoke.css"),
            LayoutKind::Unknown
        ));

        // A path under a current-layout prefix but not embedded
        // (user-original at a known structural location) is on the
        // current layout, not Unknown.
        assert!(matches!(
            classify_layout_path("static", "components/atoms/my-widget.js"),
            LayoutKind::OnCurrentLayout
        ));

        // A real new-layout file is on the current layout.
        assert!(matches!(
            classify_layout_path("static", "styles/main.css"),
            LayoutKind::OnCurrentLayout
        ));
    }

    /// Current-layout prefix matcher recognizes the new top-level dirs
    /// and rejects unrelated paths. Future-proofs Phases B–E: when
    /// `static/styles/`, `static/vendor/`, `templates/pages/`, etc.
    /// land, paths under them must not be misreported as "unknown."
    #[test]
    fn current_layout_prefixes_recognized() {
        let cases = [
            ("static", "components/atoms/foo.js", true),
            ("static", "styles/main.css", true),
            ("static", "vendor/htmx.js", true),
            ("static", "icons/logo.svg", true),
            ("static", "fonts/Geist.woff2", true),
            ("templates", "pages/dashboard/index.hbs", true),
            ("templates", "slots/dashboard_widgets/foo.hbs", true),
            ("templates", "email/reset-password.hbs", true),
            // Page-family folders (no `pages/` umbrella per Phase E
            // research): auth, collections, dashboard, errors, globals
            // all stay flat as siblings of layout/, partials/, etc.
            // User-original files at these paths (e.g. an extra
            // `templates/auth/banner.hbs`) must NOT be flagged Unknown.
            ("templates", "auth/banner.hbs", true),
            ("templates", "collections/items_table_extra.hbs", true),
            ("templates", "dashboard/welcome.hbs", true),
            ("templates", "errors/maintenance.hbs", true),
            ("templates", "globals/extras.hbs", true),
            ("static", "totally-bespoke.css", false),
            ("templates", "totally-bespoke.hbs", false),
        ];
        for (kind, sub, expected) in cases {
            assert_eq!(
                matches_current_layout_prefix(kind, sub),
                expected,
                "{kind}/{sub}",
            );
        }
    }

    /// `collect_layout_entries` walks the config dir and skips both
    /// missing overlay roots and the `.crap-overlay/` sidecar dir.
    #[test]
    fn collect_layout_entries_handles_missing_and_hidden_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // No overlay roots → empty result, no error.
        let entries = collect_layout_entries(tmp.path()).expect("collect");
        assert!(entries.is_empty());

        // Add a sidecar metadata dir (Phase G); collector must skip it.
        let sidecar = tmp.path().join(".crap-overlay");
        fs::create_dir_all(&sidecar).unwrap();
        fs::write(sidecar.join("manifest.json"), "{}").unwrap();

        // Add a real overlay file.
        let static_dir = tmp.path().join("static");
        fs::create_dir_all(&static_dir).unwrap();
        fs::write(static_dir.join("custom.css"), "body{}").unwrap();

        let entries = collect_layout_entries(tmp.path()).expect("collect");
        let paths: Vec<_> = entries.iter().map(|e| e.rel_path.as_str()).collect();
        assert_eq!(paths, vec!["static/custom.css"]);
    }

    /// End-to-end smoke: `layout()` runs without error against an
    /// empty config dir and against one with an unknown overlay file.
    #[test]
    fn layout_command_smoke() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Empty config dir — should print "already on the current layout".
        layout(tmp.path()).expect("empty dir");

        // Add one unknown user-original file.
        let static_dir = tmp.path().join("static");
        fs::create_dir_all(&static_dir).unwrap();
        fs::write(static_dir.join("bespoke.css"), "body{}").unwrap();
        layout(tmp.path()).expect("with unknown");
    }

    // ── customization_counts() — surfaced on `crap-cms status` ────

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
        let header = format!("{{{{!-- crap-cms:source {} --}}}}\n", crate_version);
        let extracted = format!("{}{}", header, upstream_str);
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
