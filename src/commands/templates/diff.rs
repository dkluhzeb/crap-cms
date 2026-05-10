//! `crap-cms templates diff` — show a unified-style diff between a
//! customized overlay file and the embedded upstream default.

use std::{fs, path::Path};

use anyhow::{Context as _, Result, bail};
use similar::{ChangeTag, TextDiff};

use super::helpers::{lookup_embedded, split_kind};

/// Run `crap-cms templates diff` for a single overlay path. The path is
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
    use similar::{ChangeTag, TextDiff};

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
