//! Guard: every persisting write path in `service::write` canonicalizes its
//! input (nest group fields + strip server-derived upload columns) before it
//! persists.
//!
//! The upload-column strip was added at the single create/update call sites but
//! missed on the bulk-update path, so a forged `url`/`*_url` on `update_many`
//! bypassed the per-document serve access gate (and could delete another
//! document's file). The three write bodies now share
//! `canonicalize_write_input`; this pins that any `persist_*` caller in the
//! module also calls it, so a new write path can't silently reintroduce the gap.
//!
//! Textual-scan limits apply (comments are stripped; a persist primitive not in
//! `PERSIST_FNS` would not be seen — add it here when one is introduced).

use std::fs;
use std::path::Path;

const PERSIST_FNS: &[&str] = &["persist_create", "persist_update", "persist_bulk_update"];

#[test]
fn write_paths_canonicalize_before_persist() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/service/write");

    let mut offenders: Vec<String> = Vec::new();

    for entry in fs::read_dir(&dir)
        .expect("read service/write dir")
        .flatten()
    {
        let path = entry.path();

        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }

        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };

        // Drop line-comments so a mention in a doc-comment doesn't count as a
        // call, then look for an actual `persist_*(` call site.
        let code: String = src
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");

        let persists = PERSIST_FNS.iter().any(|f| code.contains(&format!("{f}(")));

        if persists && !code.contains("canonicalize_write_input") {
            offenders.push(path.file_name().unwrap().to_string_lossy().into_owned());
        }
    }

    offenders.sort();

    assert!(
        offenders.is_empty(),
        "these `service::write` files persist without calling `canonicalize_write_input` \
         (nest group fields + strip server-derived upload columns) — a forged `url`/`*_url` \
         could reach the DB and bypass the upload serve gate:\n  {}",
        offenders.join("\n  ")
    );
}
