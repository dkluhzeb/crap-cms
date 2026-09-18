//! Guard: every function that persists a write canonicalizes its input (nest
//! group fields + strip server-derived upload columns) in the same body.
//!
//! The upload-column strip was added at the single create/update call sites but
//! missed on the bulk-update path, so a forged `url`/`*_url` on `update_many`
//! bypassed the per-document serve access gate (and could delete another
//! document's file). The write bodies now share `canonicalize_write_input`;
//! this pins that every caller of a `persist_*` primitive also calls it, so a
//! new write path can't silently reintroduce the gap.
//!
//! The scan is PER FUNCTION, over all of `src/` recursively. A whole-file
//! `contains` check would let a second persisting function in an existing file
//! ride on a sibling's canonicalization, and a non-recursive scan of one
//! directory would miss the primitives' own module and the operation layer
//! entirely.
//!
//! Textual-scan limits apply. Comments and test modules are removed first (see
//! `common::production_code`). A function body is delimited by brace matching
//! from the `{` that follows its signature, so a macro that emits an unbalanced
//! brace outside a literal would confuse it; the inventory floor below catches
//! the resulting shortfall. A persist primitive not in `PERSIST_FNS` would not
//! be seen — add it here when one is introduced.

mod common;

use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::common::production_code;

/// The primitives that materialize a write into the database.
const PERSIST_FNS: &[&str] = &[
    "persist_create",
    "persist_update",
    "persist_bulk_update",
    "persist_draft_version",
    "persist_unpublish",
];

/// The one canonicalization chokepoint every persisting body must run.
const CANONICALIZER: &str = "canonicalize_write_input";

/// The update admission step, which runs the canonicalizer first; a body that
/// admits its input through it is canonicalized. Pinned by
/// `the_admission_step_canonicalizes_first`.
const ADMISSION: &str = "admit_update";

/// (file, function, why it persists without canonicalizing). Reviewed, one
/// reason per row — never widen the matcher to make a body pass.
const ALLOWLIST: &[(&str, &str, &str)] = &[(
    "src/service/collections/unpublish.rs",
    "unpublish_document_in_conn",
    "flips `_status` on the stored row; takes an id, carries no caller-supplied field data",
)];

/// The known persisting bodies: single create, single update, bulk update,
/// unpublish. A scan that finds fewer has stopped seeing write paths.
const MIN_PERSIST_CALLERS: usize = 4;

/// Recursively collect every `.rs` file under `dir`.
fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `path` relative to the crate root, in forward-slash form.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// True for a character that can appear in an identifier.
fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// True when `chars[i..]` starts with `word` and neither neighbour is an
/// identifier character.
fn word_at(chars: &[char], i: usize, word: &str) -> bool {
    if i > 0 && is_ident(chars[i - 1]) {
        return false;
    }

    let spelled = word
        .chars()
        .enumerate()
        .all(|(n, c)| chars.get(i + n) == Some(&c));

    spelled && chars.get(i + word.len()).is_none_or(|c| !is_ident(*c))
}

/// Index of the `{` that opens a function body, scanning from the end of its
/// name. `None` for a bodyless declaration (a trait method signature), whose
/// `;` arrives first.
fn body_start(chars: &[char], from: usize) -> Option<usize> {
    let mut depth = 0i32;

    for (i, c) in chars.iter().enumerate().skip(from) {
        match *c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ';' if depth == 0 => return None,
            '{' if depth == 0 => return Some(i),
            _ => {}
        }
    }

    None
}

/// Index just past the `}` matching the brace at `open`.
fn body_end(chars: &[char], open: usize) -> usize {
    let mut depth = 0i32;

    for (i, c) in chars.iter().enumerate().skip(open) {
        match *c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
    }

    chars.len()
}

/// `(name, body)` for every `fn name(…) { … }` in scrubbed `code`, nested
/// functions included.
fn fn_bodies(code: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        if !word_at(&chars, i, "fn") {
            i += 1;
            continue;
        }

        let mut j = i + 2;
        while chars.get(j).is_some_and(|c| c.is_whitespace()) {
            j += 1;
        }

        let name_start = j;
        while chars.get(j).is_some_and(|c| is_ident(*c)) {
            j += 1;
        }

        if j == name_start {
            // `fn(u32) -> bool` is a function-pointer type, not a definition.
            i += 2;
            continue;
        }

        let Some(open) = body_start(&chars, j) else {
            i = j;
            continue;
        };

        let end = body_end(&chars, open);
        out.push((
            chars[name_start..j].iter().collect(),
            chars[open..end].iter().collect(),
        ));

        i = open + 1;
    }

    out
}

/// Every function in `code` that calls a persist primitive, paired with
/// whether it canonicalizes its input in the same body. The primitives' own
/// definitions are not call sites — canonicalization is their callers'
/// contract.
fn persisting_fns(code: &str) -> Vec<(String, bool)> {
    fn_bodies(code)
        .into_iter()
        .filter(|(name, body)| {
            !PERSIST_FNS.contains(&name.as_str())
                && PERSIST_FNS.iter().any(|p| body.contains(&format!("{p}(")))
        })
        .map(|(name, body)| {
            let canonicalizes =
                body.contains(CANONICALIZER) || body.contains(&format!("{ADMISSION}("));
            (name, canonicalizes)
        })
        .collect()
}

/// What the tree scan found.
struct Scan {
    /// `file::function` for each body that persists without canonicalizing.
    offenders: Vec<String>,
    /// The `ALLOWLIST` rows that matched such a body.
    reviewed: Vec<(&'static str, &'static str)>,
    /// How many bodies call a persist primitive at all.
    callers: usize,
}

/// Scan every `.rs` file under `root/src` for persisting function bodies.
fn scan_src(root: &Path) -> Scan {
    let mut files = Vec::new();
    rs_files(&root.join("src"), &mut files);
    files.sort();

    let mut scan = Scan {
        offenders: Vec::new(),
        reviewed: Vec::new(),
        callers: 0,
    };

    for path in &files {
        let Ok(src) = fs::read_to_string(path) else {
            continue;
        };

        let rel = relative(root, path);

        for (name, canonicalizes) in persisting_fns(&production_code(&src)) {
            scan.callers += 1;

            if canonicalizes {
                continue;
            }

            match ALLOWLIST
                .iter()
                .find(|(file, func, _)| *file == rel && *func == name)
            {
                Some((file, func, _)) => scan.reviewed.push((file, func)),
                None => scan.offenders.push(format!("{rel}::{name}")),
            }
        }
    }

    scan
}

/// A persisting body may delegate to the admission step only because that
/// step canonicalizes before anything else reads the input.
#[test]
fn the_admission_step_canonicalizes_first() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/service/write/admit.rs");
    let src = production_code(&fs::read_to_string(path).expect("admit.rs"));
    let body = src
        .split_once(&format!("fn {ADMISSION}("))
        .expect("the admission step exists")
        .1;
    let canonicalize_at = body
        .find(&format!("{CANONICALIZER}("))
        .expect("it canonicalizes");
    let adopt_at = body
        .find("adopt_pending_draft(")
        .expect("it adopts the draft");

    assert!(
        canonicalize_at < adopt_at,
        "the admission step must canonicalize before the draft is adopted"
    );
}

#[test]
fn write_paths_canonicalize_before_persist() {
    let Scan {
        offenders,
        reviewed,
        callers,
    } = scan_src(Path::new(env!("CARGO_MANIFEST_DIR")));

    assert!(
        offenders.is_empty(),
        "these functions call a `persist_*` primitive without calling \
         `{CANONICALIZER}` (nest group fields + strip server-derived upload columns) — a \
         forged `url`/`*_url` could reach the DB and bypass the upload serve gate:\n  {}\n\n\
         Canonicalize in the same body, or (if the body genuinely carries no caller-supplied \
         field data) add it to ALLOWLIST in tests/write_path_canonicalize_guard.rs with its \
         reason.",
        offenders.join("\n  ")
    );

    assert!(
        callers >= MIN_PERSIST_CALLERS,
        "the scan found {callers} persisting functions, fewer than the {MIN_PERSIST_CALLERS} \
         reviewed write paths — a write path was removed, or the scan stopped seeing function \
         bodies (check `fn_bodies`)."
    );

    assert_eq!(
        reviewed.len(),
        ALLOWLIST.len(),
        "ALLOWLIST rows that no longer match a persisting function: {:?}. Remove the stale rows \
         so the inventory does not rot into a vacuous pin.",
        ALLOWLIST
            .iter()
            .map(|(file, func, _)| (*file, *func))
            .filter(|row| !reviewed.contains(row))
            .collect::<Vec<_>>()
    );
}

/// Positive control: the scan the guard runs flags a persisting body that
/// skips canonicalization, and clears one that does not — including through
/// the textual hazards the scrubber exists for (braces and a persist call
/// inside a string literal, a persist call named only in a comment).
#[test]
fn scan_flags_a_persist_without_canonicalization() {
    const FIXTURE: &str = r#"
        fn forged_update(ctx: &ServiceContext, input: WriteInput<'_>) -> Result<()> {
            let msg = "persist_update( { unbalanced";
            persist_update(ctx, id, &input.data, &opts)?;
            Ok(())
        }

        fn canonical_update(ctx: &ServiceContext, mut input: WriteInput<'_>) -> Result<()> {
            canonicalize_write_input(&mut input, def);
            persist_update(ctx, id, &input.data, &opts)?;
            Ok(())
        }

        fn mentions_only(ctx: &ServiceContext) -> Result<()> {
            // persist_create( is named here but never called
            Ok(())
        }
    "#;

    let found = persisting_fns(&production_code(FIXTURE));

    assert_eq!(
        found,
        vec![
            ("forged_update".to_string(), false),
            ("canonical_update".to_string(), true),
        ],
        "the scan must flag the body that persists without canonicalizing, clear the one that \
         canonicalizes, and ignore a comment-only mention"
    );
}
