//! Guard: every MCP tool scrubs `Internal`/`Transient` service errors before
//! returning them to the client.
//!
//! MCP tool results reach the client verbatim (`format!("Error: {e}")` in the
//! server), so a tool that lets a `ServiceError` reach client text unscrubbed
//! leaks raw backend/driver detail (DB identifiers, pool vocabulary) — exactly
//! what the gRPC and REST surfaces already hide via `into_anyhow_scrubbed` /
//! `Status::from`. `ServiceError`'s `Display` prints the full `{e:#}` chain for
//! `Internal`/`Transient`, so *every* route from such a value into text is a
//! leak, not just the unscrubbed `into_anyhow`: formatting it, `to_string`ing
//! it, wrapping it in an anyhow `context`, or propagating it with `?` (it
//! implements `std::error::Error`, so `?` converts it into an `anyhow::Error`
//! carrying that same `Display`).
//!
//! This pins the whole `src/mcp/tools` tree so a new tool can't reintroduce any
//! of those forms. Textual-scan limits apply (see `surface_parity.rs`): the
//! producer and the sink must appear on the same line, so a leak staged across
//! two statements is out of reach.

use std::{
    fs,
    path::{Path, PathBuf},
};

mod common;

use common::{is_test_module_file, production_code};

/// Minimum `.rs` files the scan must find under `src/mcp/tools`. Without the
/// floor, moving or renaming the tool tree leaves the scan walking an empty
/// directory and passing vacuously. Well below the real count, so ordinary
/// refactors don't trip it.
const TOOL_FILE_FLOOR: usize = 20;

/// Expressions yielding a `ServiceError` whose `Display` can carry raw backend
/// text. `into_service_error` / `reclassify` return an arbitrary variant —
/// `Internal` and `Transient` among them — and those two variants are also
/// listed where they are constructed by name. The typed variants (`NotFound`,
/// `AccessDenied`, `Validation`, …) carry only their own message and are meant
/// to reach the client, so naming one is not a leak.
const RAW_ERROR_PRODUCERS: &[&str] = &[
    "into_service_error()",
    ".reclassify(",
    "ServiceError::Internal(",
    "ServiceError::Transient(",
];

/// Sinks that turn a value into client-visible text.
const TEXT_SINKS: &[&str] = &["anyhow!(", "format!(", ".to_string()", ".context(", "?"];

/// Which leak form (if any) one line of code commits.
///
/// Extracted from the scan so the positive control can prove it still fires on
/// each form — and, just as importantly, stays silent on the scrubbed form and
/// on a `ServiceError` merely bound for a `match`.
fn leak_form(code: &str) -> Option<&'static str> {
    if code.contains("into_anyhow") {
        return (!code.contains("into_anyhow_scrubbed")).then_some("unscrubbed `into_anyhow`");
    }

    let produces_raw = RAW_ERROR_PRODUCERS.iter().any(|p| code.contains(p));
    let reaches_client = TEXT_SINKS.iter().any(|s| code.contains(s));

    (produces_raw && reaches_client).then_some("raw ServiceError text reaches the client")
}

/// Recursively collect every production `.rs` file under `dir` — an
/// out-of-line test module is test code.
fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") && !is_test_module_file(&path) {
            out.push(path);
        }
    }
}

/// Every leaking line in one file, as `path:line — form`.
fn leaks_in_file(root: &Path, path: &Path) -> Vec<String> {
    let Ok(src) = fs::read_to_string(path) else {
        return Vec::new();
    };

    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");

    // Comments and test code are scrubbed line for line, so prose naming a
    // leak form is not scanned and reported lines stay real.
    production_code(&src)
        .lines()
        .enumerate()
        .filter_map(|(i, line)| leak_form(line).map(|form| format!("{rel}:{} — {form}", i + 1)))
        .collect()
}

#[test]
fn mcp_tools_scrub_service_errors() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rs_files(&root.join("src/mcp/tools"), &mut files);

    assert!(
        files.len() >= TOOL_FILE_FLOOR,
        "src/mcp/tools yielded {} .rs file(s), below the floor of \
         {TOOL_FILE_FLOOR} — the tool tree was moved or emptied and this scan \
         is now vacuous",
        files.len()
    );

    let mut offenders: Vec<String> = files
        .iter()
        .flat_map(|path| leaks_in_file(root, path))
        .collect();
    offenders.sort();

    assert!(
        offenders.is_empty(),
        "MCP tool(s) let a ServiceError reach the client as raw text, leaking \
         backend/driver detail. Scrub it with `into_anyhow_scrubbed` \
         (src/service/error.rs) like every other MCP tool:\n  {}",
        offenders.join("\n  ")
    );
}

/// Positive control: the matcher must fire on every leak form it claims to
/// cover. The single-token `into_anyhow` check this guard started with saw only
/// the first of these; the other three reach the client with the same raw text.
#[test]
fn leak_matcher_fires_on_every_leak_form() {
    let leaks = [
        "    .map_err(|e| e.into_service_error().into_anyhow())?;",
        "    .map_err(|e| anyhow!(\"{}\", e.into_service_error()))?;",
        "    .map_err(|e| e.into_service_error().to_string())?;",
        "    let doc = run(&ctx).map_err(CoreError::into_service_error())?;",
        "    Err(ServiceError::Internal(e)).context(\"read failed\")?;",
    ];

    for line in leaks {
        assert!(
            leak_form(line).is_some(),
            "leak form must be flagged: {line}"
        );
    }
}

/// Negative control: the scrubbed form and the legitimate non-text uses of a
/// `ServiceError` must stay silent, or the guard is noise and gets suppressed.
#[test]
fn leak_matcher_accepts_the_scrubbed_form() {
    let clean = [
        "    .map_err(|e| e.into_service_error().into_anyhow_scrubbed())?;",
        "        .map_err(ServiceError::into_anyhow_scrubbed)?;",
        "        Err(e.into_anyhow_scrubbed()).context(format!(\"Failed: {slug}\"))",
        // Converting for a `match` is not a sink; the arms below do the scrubbing.
        "    match result.map_err(op::CoreError::into_service_error) {",
        "        Err(ServiceError::Internal(e)) if is_uninitialized_global(&e) => {",
        // An unrelated error formatted into anyhow is not a ServiceError leak.
        "        .map_err(|e| anyhow!(e))?;",
        "        .ok_or_else(|| anyhow::anyhow!(\"Collection not found\"))?;",
    ];

    for line in clean {
        assert!(
            leak_form(line).is_none(),
            "compliant line must not be flagged: {line}"
        );
    }
}
