//! Architectural guard: **one implementation per chokepoint**.
//!
//! A handful of decisions in this codebase are deliberately made in exactly one
//! place — how a locale context is built, what a reader may not see, how a
//! number spelled as text is read, what a hard delete entails. Every one of them
//! was, at some point, spelled out a second time somewhere else and the two
//! copies drifted: a context built without the fallback the reader expected, a
//! hidden field stripped on one surface but not another, a purge that forgot the
//! files. The fix each time was the same — route the second caller through the
//! one implementation.
//!
//! This test is the reviewed inventory of what is left. Each chokepoint carries
//! the textual shape a *copy* of it takes, and the list of production files that
//! shape may still appear in, with the reason. A new file matching the shape
//! fails here and forces the decision: call the chokepoint, or add the file to
//! the chokepoint's allowlist with its justification.
//!
//! **Scope & limits.** The scans are per-line regex over the source text, not
//! AST-based: a call split across two lines, reached through an alias
//! (`use x as y`), or built by a macro will not match. Test code is excluded by
//! truncating each file at its test module — the first `#[cfg(…test…)]`
//! attribute that gates something other than a single `use` or a file-based
//! `mod …;`, which the scan continues past — and by skipping test-only files
//! (`tests.rs`, `*_tests.rs`, `test_*.rs`, `*_test.rs`). Treat these as a
//! high-signal tripwire for the obvious regression, not a proof of total
//! coverage.

use std::{
    fs,
    path::{Path, PathBuf},
};

use regex::Regex;

// ── the inventory ────────────────────────────────────────────────────────────

/// `LocaleContext::{default_for, exact, from_locale_string}` own how a locale
/// context is built — including the parts a literal forgets, such as the
/// fallback `exact` turns off so a snapshot read takes only what the locale
/// itself holds.
const LOCALE_CONTEXT: Chokepoint = Chokepoint {
    name: "LocaleContext::{default_for, exact, from_locale_string}",
    scan_root: "src",
    home: Some("src/db/query/locale"),
    copy_pattern: r"\bLocaleContext \{|\bLocaleMode::Single\(",
    fix: "Build the context with `LocaleContext::default_for` / `::exact` / \
          `::from_locale_string` instead of a struct literal, so the mode and \
          the fallback are decided in one place.",
    allowlist: &[
        (
            "src/commands/export/import_cmd.rs",
            "Import restores one snapshot locale at a time: a Single context per locale key",
        ),
        (
            "src/db/query/versions/restore/join_rows.rs",
            "Restore writes each locale's join rows under its own Single context",
        ),
        (
            "src/db/query/versions/snapshot.rs",
            "All-locales snapshot read — `LocaleMode::All` has no named constructor",
        ),
        (
            "src/service/write/update.rs",
            "Destructuring pattern on the caller's context (locale + config), not a construction",
        ),
    ],
};

/// `service::helpers::strip_unreadable{,_docs}` is the one strip a read passes
/// through: the data-aware read-access hooks first, then the API-hidden fields.
/// A caller that collects the hidden names itself runs one half of that.
const HIDDEN_FIELD_STRIP: Chokepoint = Chokepoint {
    name: "service::helpers::strip_unreadable / strip_unreadable_docs",
    scan_root: "src",
    home: None,
    copy_pattern: r"collect_api_hidden_field_names\(",
    fix: "Strip the document through `service::helpers::strip_unreadable` (or \
          `strip_unreadable_docs` for a batch), which runs the read-access \
          hooks before the hidden fields — collecting the hidden names alone \
          skips the data-aware half.",
    allowlist: &[
        (
            "src/service/helpers.rs",
            "The chokepoint itself: both strip helpers and the collector",
        ),
        (
            "src/service/read/validate_filters.rs",
            "Filter validation rejects a filter on a hidden field before any document is read",
        ),
        (
            "src/service/read/populated_strip.rs",
            "Populated targets are stripped per target collection, with the names memoized",
        ),
    ],
};

/// `core::parse_number` is the one reading of a number spelled as text, so a
/// value the write stores can never be a value validation rejects.
const PARSE_NUMBER: Chokepoint = Chokepoint {
    name: "core::parse_number",
    scan_root: "src",
    home: None,
    copy_pattern: r"parse::<f64>\(\)",
    fix: "Read the number with `core::parse_number`, so the write edge, \
          validation and the filter edge agree on what text is a number.",
    allowlist: &[("src/core/parse.rs", "The chokepoint itself")],
};

/// `service::purge_document` is what a hard delete means: the row, its join
/// rows, its versions, its files, and the reference counts it was holding.
const PURGE_DOCUMENT: Chokepoint = Chokepoint {
    name: "service::purge_document",
    scan_root: "src",
    home: None,
    copy_pattern: r"query::delete\(",
    fix: "Hard-delete through `service::purge_document`, which drops the \
          document's files and releases the references it holds — the raw \
          row delete does neither.",
    allowlist: &[(
        "src/service/write/delete.rs",
        "The chokepoint itself: `purge_document` issues the row delete",
    )],
};

/// `admin::handlers::shared::locale::editor_locale_ctx` is how an admin request
/// turns the editor's locale into a read context — including the decision that
/// an unknown locale reads the default rather than dropping the context (a
/// `None` context on a localized collection selects columns that don't exist).
const EDITOR_LOCALE_CTX: Chokepoint = Chokepoint {
    name: "admin::handlers::shared::editor_locale_ctx",
    scan_root: "src/admin/handlers",
    home: None,
    copy_pattern: r"\bLocaleContext::(from_locale_string|default_for|exact)\(|\bLocaleContext \{",
    fix: "Build the admin read context with `editor_locale_ctx` (or \
          `parse_request_locale` when an unknown locale must 400), so a stale \
          cookie still reads the default locale instead of dropping the \
          context and selecting columns that don't exist.",
    allowlist: &[
        (
            "src/admin/handlers/shared/locale.rs",
            "The chokepoint itself, and the strict `parse_request_locale` variant beside it",
        ),
        (
            "src/admin/handlers/field_context/enrich/enrichment.rs",
            "Relationship labels fall back to the default locale when the caller passes no \
             context — the editor's locale is already resolved upstream",
        ),
        (
            "src/admin/handlers/uploads/serve.rs",
            "The file-serve gate resolves the owning row, not a translation: no request locale \
             is in scope, and the default-locale context only keeps the SELECT valid",
        ),
    ],
};

/// `core::field::companion` owns the companion suffixes. They are both a column
/// name and a reserved field-name ending; spelling one out again lets column
/// generation and name reservation drift, and a user field named `starts_tz`
/// silently collides with a date's zone column.
const COMPANION_SUFFIX: Chokepoint = Chokepoint {
    name: "core::{TZ_SUFFIX, LANG_SUFFIX}",
    scan_root: "src",
    home: None,
    copy_pattern: r#""_tz"|"_lang"|_tz"\)|_lang"\)|\{\}_tz|\{\}_lang"#,
    fix: "Use `TZ_SUFFIX` / `LANG_SUFFIX` (and the `tz_column` / `lang_column` \
          builders over them), so the column a write creates and the field name \
          the parser reserves can't drift apart.",
    allowlist: &[(
        "src/core/field/companion.rs",
        "The chokepoint itself: the two companion-suffix constants",
    )],
};

/// `core::upload::shape_read_document` is the one place an upload document
/// takes the shape a read returns (the per-size values folded into `sizes`).
/// A caller that folds the sizes itself takes today's shape and misses the
/// next step added there.
const SHAPE_READ_DOCUMENT: Chokepoint = Chokepoint {
    name: "core::upload::shape_read_document",
    scan_root: "src",
    home: None,
    copy_pattern: r"assemble_sizes_object\(",
    fix: "Shape the document with `core::upload::shape_read_document(def, doc)`, \
          which applies every read-shape step an upload document needs, \
          instead of folding the sizes yourself.",
    allowlist: &[(
        "src/core/upload/metadata.rs",
        "The chokepoint itself: `shape_read_document` folds the sizes there",
    )],
};

/// Every chokepoint, for the allowlist-staleness companion test.
const CHOKEPOINTS: &[&Chokepoint] = &[
    &LOCALE_CONTEXT,
    &HIDDEN_FIELD_STRIP,
    &PARSE_NUMBER,
    &PURGE_DOCUMENT,
    &EDITOR_LOCALE_CTX,
    &COMPANION_SUFFIX,
    &SHAPE_READ_DOCUMENT,
];

// ── the shared scan ──────────────────────────────────────────────────────────

/// One chokepoint, the textual shape a copy of it takes, and the reviewed
/// production files that shape may still appear in.
struct Chokepoint {
    /// The single implementation, named in the failure message.
    name: &'static str,
    /// Directory walked, relative to the crate root.
    scan_root: &'static str,
    /// The chokepoint's own module, if it has one — everything below it *is*
    /// the implementation and is never a copy.
    home: Option<&'static str>,
    /// Regex matching a line that re-implements the chokepoint.
    copy_pattern: &'static str,
    /// What to do instead.
    fix: &'static str,
    /// `(path, why this one is not a copy)`.
    allowlist: &'static [(&'static str, &'static str)],
}

/// One production line matching a chokepoint's copy pattern.
struct Hit {
    path: String,
    line: usize,
    text: String,
}

impl Chokepoint {
    /// Every production line under `scan_root` (outside `home`) matching the
    /// copy pattern, allowlisted or not.
    fn hits(&self) -> Vec<Hit> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let pattern = Regex::new(self.copy_pattern).expect("chokepoint pattern compiles");

        let mut files = Vec::new();
        rs_files(&root.join(self.scan_root), &mut files);
        files.sort();

        let mut hits = Vec::new();

        for file in files {
            let rel = relative(root, &file);

            if self.home.is_some_and(|home| rel.starts_with(home)) {
                continue;
            }

            let Ok(src) = fs::read_to_string(&file) else {
                continue;
            };

            for (idx, line) in production(&src).lines().enumerate() {
                let trimmed = line.trim_start();

                if trimmed.starts_with("//") || trimmed.starts_with('*') {
                    continue;
                }

                if pattern.is_match(line) {
                    hits.push(Hit {
                        path: rel.clone(),
                        line: idx + 1,
                        text: trimmed.to_string(),
                    });
                }
            }
        }

        hits
    }

    /// Fail with the chokepoint to use when a file outside the allowlist
    /// re-implements it.
    fn assert_no_copies(&self) {
        let offenders: Vec<String> = self
            .hits()
            .iter()
            .filter(|hit| !self.allowlist.iter().any(|(path, _)| *path == hit.path))
            .map(|hit| format!("  {}:{}  {}", hit.path, hit.line, hit.text))
            .collect();

        assert!(
            offenders.is_empty(),
            "Production code re-implements `{}`:\n{}\n\n{}\n\nIf this one genuinely cannot go \
             through the chokepoint, add it to that chokepoint's allowlist in \
             tests/chokepoint_copies.rs with the reason.",
            self.name,
            offenders.join("\n"),
            self.fix
        );
    }

    /// Fail when an allowlist row no longer describes anything, so the
    /// inventory can't rot into a set of vacuous pins.
    fn assert_allowlist_is_live(&self) {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let hits = self.hits();

        for (rel, reason) in self.allowlist {
            assert!(
                root.join(rel).exists(),
                "Allowlisted file no longer exists: {rel} ({reason}), under chokepoint `{}`. \
                 Remove the stale row in tests/chokepoint_copies.rs.",
                self.name
            );

            assert!(
                hits.iter().any(|hit| hit.path == *rel),
                "Allowlisted file {rel} no longer matches the copy pattern of chokepoint `{}`. \
                 What it documented ({reason}) is gone — remove the stale row in \
                 tests/chokepoint_copies.rs.",
                self.name
            );
        }
    }
}

/// Collect every `.rs` file under `dir` that is not test-only, recursively.
fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") && !is_test_only_file(&path) {
            out.push(path);
        }
    }
}

/// True for a file that exists only for tests — a `#[cfg(test)] mod` in its own
/// file carries no gate of its own to truncate at.
fn is_test_only_file(path: &Path) -> bool {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };

    stem == "tests"
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
}

/// The part of `src` above its test module — everything before the first
/// `#[cfg(…test…)]` attribute that gates more than a single item.
fn production(src: &str) -> &str {
    let lines: Vec<&str> = src.split_inclusive('\n').collect();
    let mut offset = 0;

    for (idx, line) in lines.iter().enumerate() {
        if is_test_cfg(line) && !gates_one_item(&lines[idx + 1..]) {
            return &src[..offset];
        }

        offset += line.len();
    }

    src
}

/// True for a `#[cfg(test)]` attribute, including the
/// `#[cfg(all(test, feature = "sqlite"))]` spelling several modules use.
fn is_test_cfg(line: &str) -> bool {
    let trimmed = line.trim_start();

    trimmed.starts_with("#[cfg(") && trimmed.contains("test")
}

/// True when the attribute above `rest` gates a single item production code
/// continues after — a test-only `use`, or a test module living in its own file
/// (`mod tests;`). Anything else is the file's inline test module (often behind
/// a stacked `#[allow(…)]`), and with it the rest of the file.
fn gates_one_item(rest: &[&str]) -> bool {
    rest.iter()
        .map(|line| line.trim())
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.starts_with("use ") || line.ends_with(';'))
}

/// `path` relative to the crate root, in forward-slash form.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

// ── one test per chokepoint ──────────────────────────────────────────────────

#[test]
fn locale_contexts_are_built_by_the_locale_module() {
    LOCALE_CONTEXT.assert_no_copies();
}

#[test]
fn reads_strip_hidden_fields_through_the_service_helper() {
    HIDDEN_FIELD_STRIP.assert_no_copies();
}

#[test]
fn numbers_spelled_as_text_are_read_by_parse_number() {
    PARSE_NUMBER.assert_no_copies();
}

#[test]
fn hard_deletes_go_through_purge_document() {
    PURGE_DOCUMENT.assert_no_copies();
}

#[test]
fn admin_handlers_build_the_editor_locale_context_once() {
    EDITOR_LOCALE_CTX.assert_no_copies();
}

#[test]
fn companion_suffixes_are_spelled_once() {
    COMPANION_SUFFIX.assert_no_copies();
}

#[test]
fn upload_documents_take_their_read_shape_in_one_place() {
    SHAPE_READ_DOCUMENT.assert_no_copies();
}

#[test]
fn allowlisted_files_still_match_their_chokepoint() {
    for chokepoint in CHOKEPOINTS {
        chokepoint.assert_allowlist_is_live();
    }
}

/// Positive control: the scan must still fire on a shape it is supposed to
/// catch. A pattern that stops matching anything (a renamed call, a reworked
/// literal) would leave every test above passing on an empty scan.
#[test]
fn every_chokepoint_scan_still_matches_something() {
    for chokepoint in CHOKEPOINTS {
        assert!(
            !chokepoint.hits().is_empty(),
            "The copy pattern of chokepoint `{}` matches nothing in production code. \
             It scans for a shape that no longer exists — update the pattern in \
             tests/chokepoint_copies.rs or drop the chokepoint.",
            chokepoint.name
        );
    }
}
