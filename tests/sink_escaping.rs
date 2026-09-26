//! Sink-escaping inventory.
//!
//! Every place an untrusted value crosses into an interpreter or
//! protocol — HTML, JSON-in-markup, SQL, Lua source, email headers,
//! filesystem paths — must route through that sink's escaper or
//! validator. The failures this class collects were never missing
//! escapers; they were *sites that didn't use them* or used the wrong
//! variant (`html_escape` where the value landed in an attribute).
//!
//! This file is the reviewed inventory: one row per (sink, anchor,
//! escaping call). The test pins each escaper as live at its anchor — an
//! escaper that gets renamed or deleted fails here and forces the
//! inventory (and every call site) through review.
//!
//! A needle must name the escaping call itself (`fn html_escape(s: &str)`,
//! `.replace('\'', r"\u0027")`), never a fragment a passing mention could
//! satisfy: a bare `'` is present in any file with an apostrophe in a
//! comment, which pins nothing. Rust anchors are scanned with comments and
//! test modules removed (see `common::production_code`), so neither a doc
//! comment nor a copy of the escaping chain inside `#[cfg(test)]` can keep
//! a deleted escaper "alive". Textual-scan limits apply, as documented in
//! `surface_parity.rs`.

mod common;

use std::{fs, path::Path};

use crap_cms::core::email::validate_no_crlf;
use tempfile::tempdir;

use crate::common::{is_test_module_file, production_code};

/// (sink, anchor file, the escaping call that must be live there)
const SINK_INVENTORY: &[(&str, &str, &str)] = &[
    (
        "HTML text content (richtext render)",
        "src/core/richtext/renderer.rs",
        "fn html_escape(s: &str)",
    ),
    (
        "HTML attribute values (quote-escaping variant)",
        "src/core/richtext/renderer.rs",
        "fn html_escape_attr(s: &str)",
    ),
    (
        "JSON embedded in <script> ({{{json}}} helper)",
        "src/admin/templates/helpers/json.rs",
        r#".replace("</", r"<\/")"#,
    ),
    (
        "JSON embedded in single-quoted attributes ({{{json}}} helper)",
        "src/admin/templates/helpers/json.rs",
        r#".replace('\'', r"\u0027")"#,
    ),
    (
        "JSON embedded in attributes the parser entity-decodes ({{{json}}} helper)",
        "src/admin/templates/helpers/json.rs",
        r#".replace('&', r"\u0026")"#,
    ),
    (
        "JSON i18n island (second raw-JSON producer — shares the json.rs escaper)",
        "src/admin/templates/helpers/admin_i18n.rs",
        "markup_json(",
    ),
    (
        "SQL string literals in DDL DEFAULT clauses (placeholders can't bind DDL)",
        "src/db/migrate/collection/create.rs",
        r#"s.replace('\'', "''")"#,
    ),
    (
        "Locale fragments in DDL (strip-validator, not escaper)",
        "src/db/query/validation.rs",
        "fn sanitize_locale(locale: &str)",
    ),
    (
        "SQL identifiers (reserved words, quoting)",
        "src/db/query/helpers/sql.rs",
        "fn quote_ident(name: &str)",
    ),
    (
        "Email headers (CRLF injection)",
        "src/core/email/validation.rs",
        "fn validate_no_crlf(field_name: &str, value: &str)",
    ),
    (
        "Lua source embedding (scaffold-generated definitions)",
        "src/scaffold/collection/parser.rs",
        "fn escape_lua_string(s: &str)",
    ),
    (
        "Filesystem storage keys (traversal)",
        "src/core/upload/storage/backend.rs",
        "fn validate_key(key: &str)",
    ),
    (
        "Template render paths (traversal)",
        "src/core/field/admin.rs",
        "fn validate_template_name(name: &str)",
    ),
    (
        "Client DOM construction (no innerHTML for untrusted values)",
        "static/components/_internal/h.js",
        "el.textContent = String(v)",
    ),
];

/// The inventory only ever grows through review. A row deleted because its
/// needle went stale would take that sink's pin with it, silently — this
/// floor turns that into a failure. Raise it when rows are added.
const MIN_SINK_ROWS: usize = 14;

/// True when `needle` is live in `file`.
///
/// Rust anchors are scanned as production code only (test modules — inline
/// or in a file of their own — and comments removed), so a needle can only be
/// satisfied by code that runs.
/// The admin JS anchor is scanned as written — the scrubber's rules are
/// Rust's — so its needle carries enough context (`el.textContent =
/// String(v)`) to be code and not prose.
fn row_is_live(root: &Path, file: &str, needle: &str) -> bool {
    let path = root.join(file);

    let Ok(src) = fs::read_to_string(&path) else {
        return false;
    };

    if Path::new(file).extension().is_some_and(|ext| ext == "rs") {
        return !is_test_module_file(&path) && production_code(&src).contains(needle);
    }

    src.contains(needle)
}

/// Every inventory row's escaper is still live at its anchor.
#[test]
fn every_sink_escaper_is_live() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut dead = Vec::new();

    for (sink, file, needle) in SINK_INVENTORY {
        if !row_is_live(root, file, needle) {
            dead.push(format!("{sink}: `{needle}` missing from {file}"));
        }
    }

    assert!(
        dead.is_empty(),
        "sink-escaping inventory has dead anchors — the escaper moved or \
         was deleted; update the inventory AND audit its call sites:\n  {}",
        dead.join("\n  ")
    );
}

/// A row removed without its sink being gone leaves that sink unpinned.
#[test]
fn inventory_keeps_every_reviewed_row() {
    assert!(
        SINK_INVENTORY.len() >= MIN_SINK_ROWS,
        "SINK_INVENTORY is down to {} rows from {MIN_SINK_ROWS} — a sink lost its pin. \
         Restore the row, or (if the sink really is gone) drop the floor in the same commit \
         that removes it.",
        SINK_INVENTORY.len()
    );
}

/// Behavior pin for the one escaper reachable from integration tests:
/// CRLF/NUL rejection on email header values.
#[test]
fn email_header_validation_rejects_injection() {
    assert!(validate_no_crlf("subject", "hello world").is_ok());

    for bad in ["a\r\nBcc: x@y.z", "a\rb", "a\nb", "a\0b"] {
        assert!(
            validate_no_crlf("subject", bad).is_err(),
            "must reject {bad:?}"
        );
    }
}

/// Positive control: the liveness check the inventory runs reports a needle
/// as dead unless it is in live production code — a mention in a comment or
/// a copy inside a test module does not count.
#[test]
fn row_liveness_only_counts_production_code() {
    let dir = tempdir().expect("tempdir");
    fs::write(
        dir.path().join("fixture.rs"),
        "// fn commented_escaper(s: &str) is only named here\n\
         fn live_escaper(s: &str) -> String {\n    s.to_string()\n}\n\
         #[cfg(all(test, feature = \"sqlite\"))]\n\
         mod tests {\n    fn test_only_escaper(s: &str) {}\n}\n",
    )
    .expect("write fixture");

    assert!(
        row_is_live(dir.path(), "fixture.rs", "fn live_escaper(s: &str)"),
        "a live escaper must be found"
    );
    assert!(
        !row_is_live(dir.path(), "fixture.rs", "fn commented_escaper(s: &str)"),
        "a needle that only appears in a comment must be reported dead"
    );
    assert!(
        !row_is_live(dir.path(), "fixture.rs", "fn test_only_escaper(s: &str)"),
        "a needle that only exists under a test gate must be reported dead"
    );
    assert!(
        !row_is_live(dir.path(), "fixture.rs", "fn no_such_escaper(s: &str)"),
        "an absent needle must be reported dead"
    );
    assert!(
        !row_is_live(dir.path(), "missing.rs", "fn live_escaper(s: &str)"),
        "an unreadable anchor must be reported dead"
    );

    // A sibling file that is entirely test code carries its gate as an inner
    // attribute, with no item-level gate anywhere below it.
    fs::write(
        dir.path().join("all_tests.rs"),
        "//! Fixture module.\n#![cfg(test)]\n\nuse std::fmt;\n\nfn whole_file_escaper(s: &str) {}\n",
    )
    .expect("write fixture");

    assert!(
        !row_is_live(dir.path(), "all_tests.rs", "fn whole_file_escaper(s: &str)"),
        "a needle in a file gated by an inner `#![cfg(test)]` must be reported dead"
    );
}
