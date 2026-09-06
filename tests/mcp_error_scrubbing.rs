//! Guard: every MCP tool scrubs `Internal`/`Transient` service errors before
//! returning them to the client.
//!
//! MCP tool results reach the client verbatim (`format!("Error: {e}")` in the
//! server), so a tool that maps a `ServiceError` with the UNSCRUBBED
//! `into_anyhow` leaks raw backend/driver text (DB identifiers, pool
//! vocabulary) — exactly what the gRPC and REST surfaces already hide via
//! `into_anyhow_scrubbed` / `Status::from`. The job tools were the one
//! surface that diverged; this pins the whole `src/mcp/tools` tree so a new
//! tool can't reintroduce the leak. Textual-scan limits apply (see
//! `surface_parity.rs`).

use std::fs;
use std::path::{Path, PathBuf};

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

#[test]
fn mcp_tools_scrub_service_errors() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/mcp/tools");
    let mut files = Vec::new();
    rs_files(&root, &mut files);

    let mut offenders: Vec<String> = Vec::new();

    for path in &files {
        let Ok(src) = fs::read_to_string(path) else {
            continue;
        };

        for (i, line) in src.lines().enumerate() {
            // Drop anything after the first `//` so a doc/comment mentioning
            // `into_anyhow` isn't flagged; only real code is scanned.
            let code = line.split("//").next().unwrap_or("");

            if code.contains("into_anyhow") && !code.contains("into_anyhow_scrubbed") {
                let rel = path
                    .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                offenders.push(format!("{rel}:{}", i + 1));
            }
        }
    }

    offenders.sort();

    assert!(
        offenders.is_empty(),
        "MCP tool(s) map a ServiceError with the UNSCRUBBED `into_anyhow`, which leaks raw \
         backend text to the client. Use `into_anyhow_scrubbed` (src/service/error.rs) like \
         every other MCP tool:\n  {}",
        offenders.join("\n  ")
    );
}
