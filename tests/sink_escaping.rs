//! Sink-escaping inventory.
//!
//! Every place an untrusted value crosses into an interpreter or
//! protocol — HTML, JSON-in-markup, SQL, Lua source, email headers,
//! filesystem paths — must route through that sink's escaper or
//! validator. The failures this class collects were never missing
//! escapers; they were *sites that didn't use them* or used the wrong
//! variant (`html_escape` where the value landed in an attribute).
//!
//! This file is the reviewed inventory: one row per (sink, escaper,
//! anchor). The test pins each escaper as live at its anchor — an
//! escaper that gets renamed or deleted fails here and forces the
//! inventory (and every call site) through review. Textual-scan limits
//! apply, as documented in `surface_parity.rs`.

use std::fs;
use std::path::Path;

/// (sink, anchor file, escaper/validator symbol that must exist there)
const SINK_INVENTORY: &[(&str, &str, &str)] = &[
    (
        "HTML text content (richtext render)",
        "src/core/richtext/renderer.rs",
        "fn html_escape",
    ),
    (
        "HTML attribute values (quote-escaping variant)",
        "src/core/richtext/renderer.rs",
        "fn html_escape_attr",
    ),
    (
        "JSON embedded in <script>/attributes ({{{json}}} helper)",
        "src/admin/templates/helpers/json.rs",
        r"<\/",
    ),
    (
        "JSON embedded in single-quoted attributes",
        "src/admin/templates/helpers/json.rs",
        r"'",
    ),
    (
        "JSON i18n island (second raw-JSON producer — must mirror json.rs)",
        "src/admin/templates/helpers/admin_i18n.rs",
        r"\u0027",
    ),
    (
        "SQL string literals in DDL DEFAULT clauses (placeholders can't bind DDL)",
        "src/db/migrate/collection/create.rs",
        r#"replace('\'', "''")"#,
    ),
    (
        "Locale fragments in DDL (strip-validator, not escaper)",
        "src/db/query/validation.rs",
        "fn sanitize_locale",
    ),
    (
        "SQL identifiers (reserved words, quoting)",
        "src/db/query/helpers.rs",
        "fn quote_ident",
    ),
    (
        "Email headers (CRLF injection)",
        "src/core/email/validation.rs",
        "fn validate_no_crlf",
    ),
    (
        "Lua source embedding (scaffold-generated definitions)",
        "src/scaffold/collection/parser.rs",
        "fn escape_lua_string",
    ),
    (
        "Filesystem storage keys (traversal)",
        "src/core/upload/storage/backend.rs",
        "fn validate_key",
    ),
    (
        "Template render paths (traversal)",
        "src/core/field/admin.rs",
        "fn validate_template_name",
    ),
    (
        "Client DOM construction (no innerHTML for untrusted values)",
        "static/components/_internal/h.js",
        "textContent",
    ),
];

/// Every inventory row's escaper is still live at its anchor.
#[test]
fn every_sink_escaper_is_live() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut dead = Vec::new();

    for (sink, file, symbol) in SINK_INVENTORY {
        let present = fs::read_to_string(root.join(file))
            .ok()
            .is_some_and(|c| c.contains(symbol));
        if !present {
            dead.push(format!("{sink}: `{symbol}` missing from {file}"));
        }
    }

    assert!(
        dead.is_empty(),
        "sink-escaping inventory has dead anchors — the escaper moved or \
         was deleted; update the inventory AND audit its call sites:\n  {}",
        dead.join("\n  ")
    );
}

/// Behavior pin for the one escaper reachable from integration tests:
/// CRLF/NUL rejection on email header values.
#[test]
fn email_header_validation_rejects_injection() {
    use crap_cms::core::email::validate_no_crlf;

    assert!(validate_no_crlf("subject", "hello world").is_ok());
    for bad in ["a\r\nBcc: x@y.z", "a\rb", "a\nb", "a\0b"] {
        assert!(
            validate_no_crlf("subject", bad).is_err(),
            "must reject {bad:?}"
        );
    }
}

/// Positive control: the liveness scan fails on a
/// synthetic dead anchor.
#[test]
fn sink_scan_fires_on_synthetic_dead_anchor() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let present = fs::read_to_string(root.join("src/core/richtext/renderer.rs"))
        .ok()
        .is_some_and(|c| c.contains("fn no_such_escaper_xyz"));
    assert!(!present, "a bogus symbol must not be found");
}
