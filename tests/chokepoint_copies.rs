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
//! `common::production_code` — every `#[cfg(…test…)]`-gated item is blanked
//! (only that item: a gated helper in the middle of a file hides nothing after
//! it), comments are removed, line numbers are kept — and by skipping
//! test-only files (`tests.rs`, `*_tests.rs`, `test_*.rs`, `*_test.rs`). Treat
//! these as a high-signal tripwire for the obvious regression, not a proof of
//! total coverage.

use std::{
    fs,
    path::{Path, PathBuf},
};

use regex::Regex;

mod common;

use common::production_code;

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

/// `service::upload::{create_upload, update_upload}` is the one entry an upload
/// write goes through, and `service::write::settle_upload_write` — inside the
/// write transaction — is the one place that decides what happens to the file
/// the row stops referencing and to the conversions still queued for it. A
/// surface that spells the lifecycle out again gets one of the parts wrong: the
/// admin's copy deleted the published file on a *draft* save (every live page
/// 404s, and no version brings it back), never cancelled the replaced file's
/// conversions, which then overwrote the new file's derivative URLs, and held
/// the stored file's `CleanupGuard` in the async handler — a dropped handler
/// future deleted the bytes of a row the blocking task had already committed.
const UPLOAD_WRITE_LIFECYCLE: Chokepoint = Chokepoint {
    name: "service::upload::{create_upload, update_upload} / settle_upload_write",
    scan_root: "src",
    home: Some("src/core/upload"),
    copy_pattern: r"delete_upload_files\(|enqueue_conversions\(|process_upload\(|\bCleanupGuard\b",
    fix: "Write uploads through `service::upload::create_upload` / \
          `update_upload`. It stores the file and commits its cleanup guard in \
          one synchronous body, and what happens to the previous file and its \
          queued conversions is settled inside the write transaction by \
          `service::write::settle_upload_write`, which keys the deletion on \
          the files the row stopped referencing — not on whether the request \
          carried one.",
    allowlist: &[
        (
            "src/service/upload.rs",
            "The chokepoint itself: the only place a file is stored, and the only scope its \
             cleanup guard lives in — a synchronous body, never an async handler that can be \
             dropped mid-await while the write commits",
        ),
        (
            "src/service/write/upload_files.rs",
            "The chokepoint itself: the in-transaction file/job settlement",
        ),
    ],
};

/// `service::write::admission` owns what a write does to the caller's data
/// before the access gate judges it: canonicalize, adopt the pending draft a
/// publish makes live, apply the locale lock. The real writes and the
/// `validate` dry-run both call it; a dry-run that spelled those steps out a
/// second time drifted from the write it previews (it missed the draft, the
/// locale lock and the upload-metadata strip).
const WRITE_ADMISSION: Chokepoint = Chokepoint {
    name: "service::write::admission (admit_create_input / admit_update_input / \
           admit_global_update_input)",
    scan_root: "src/service",
    home: Some("src/service/write"),
    copy_pattern: r"\bnest_group_fields\(|\bcanonicalize_text_values\(|\badopt_pending(_global)?_draft\(|\breject_locale_locked_fields\(",
    fix: "Admit the input through `admit_create_input` / `admit_update_input` / \
          `admit_global_update_input`, so the write and its dry-run judge the same \
          canonicalized, draft-adopted, locale-locked data.",
    allowlist: &[
        (
            "src/service/persist/update.rs",
            "Post-hook locale-lock safety net over the data a before-hook produced; the \
             input itself was admitted",
        ),
        (
            "src/service/globals/update.rs",
            "Post-hook locale-lock safety net over the global's final hook data; the input \
             itself was admitted",
        ),
        (
            "src/service/versions/restore.rs",
            "Canonicalizes a stored snapshot being restored; there is no caller input to admit",
        ),
        (
            "src/service/versions/save_draft.rs",
            "Re-nests the merged draft snapshot it stores; the draft write's input was admitted",
        ),
        (
            "src/service/hooks/write.rs",
            "Nests the stored row field-access rules judge as `ctx.document`; not write input",
        ),
    ],
};

/// `commands::load_config` / `load_config_for_recovery` own how a command
/// reads its configuration: validated, with the process-wide limits installed.
/// A command that loads it by hand runs on a configuration the server would
/// refuse; skipping validation is an explicit operator flag on the recovery
/// commands, never a property of a command.
const CONFIG_LOAD: Chokepoint = Chokepoint {
    name: "commands::{load_config, load_config_for_recovery}",
    scan_root: "src",
    home: Some("src/commands/helpers.rs"),
    copy_pattern: r"\bCrapConfig::load(_unvalidated)?\(",
    fix: "Load the config through `commands::load_config` (or \
          `load_config_for_recovery` behind `--skip-config-validation` on an \
          offline recovery command), so it is validated before the command runs.",
    allowlist: &[
        (
            "src/main.rs",
            "Reads `[logging]` to set up logging before dispatch, only for `serve` / \
             `work`, which load and validate the config themselves right after",
        ),
        (
            "src/commands/make/helpers.rs",
            "Best-effort locale detection for scaffold defaults; never runs on the config",
        ),
    ],
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
    &UPLOAD_WRITE_LIFECYCLE,
    &WRITE_ADMISSION,
    &CONFIG_LOAD,
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

            for (idx, line) in production_code(&src).lines().enumerate() {
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
    if path.components().any(|c| c.as_os_str() == "tests") {
        return true;
    }

    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };

    stem == "tests"
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem.ends_with("_tests")
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
fn upload_writes_settle_their_files_and_jobs_in_one_place() {
    UPLOAD_WRITE_LIFECYCLE.assert_no_copies();
}

#[test]
fn writes_and_their_dry_run_admit_input_in_one_place() {
    WRITE_ADMISSION.assert_no_copies();
}

/// The `validate` dry-run previews a write only because it runs that write's
/// admission: pin that its body calls every admission step.
#[test]
fn the_validate_dry_run_runs_the_write_admission() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/service/op/validate.rs");
    let src = production_code(&fs::read_to_string(path).expect("validate.rs"));

    for step in [
        "admit_create_input(",
        "admit_update_input(",
        "admit_global_update_input(",
    ] {
        assert!(
            src.contains(step),
            "the validate dry-run no longer calls `{step}` — it must admit its input \
             through the same step the write it previews runs"
        );
    }
}

#[test]
fn commands_load_their_config_validated() {
    CONFIG_LOAD.assert_no_copies();
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

// ── write transactions come from the write pool ──────────────────────────────

/// How many statements above a `transaction_immediate()` the scan looks for the
/// connection it runs on. A checkout further away than this — or a connection
/// arriving as a function parameter — is out of the scan's reach; the caller's
/// own site is where that one gets pinned.
const CHECKOUT_WINDOW: usize = 25;

/// `(path, why a read checkout there is not a write transaction)`. Empty: every
/// `BEGIN IMMEDIATE` in the tree takes its connection from the write pool.
const READ_POOL_WRITE_TX_ALLOWLIST: &[(&str, &str)] = &[];

/// One statement of production source, with the line it starts on. A method
/// chain broken across lines is joined back into one entry, so a checkout
/// spelled `pool` / `.write()` on two lines reads as `pool.write()`.
struct Statement {
    line: usize,
    text: String,
}

fn statements(src: &str) -> Vec<Statement> {
    let mut out: Vec<Statement> = Vec::new();

    for (idx, line) in production_code(src).lines().enumerate() {
        let trimmed = line.trim();

        match out.last_mut() {
            Some(last) if trimmed.starts_with('.') => last.text.push_str(trimmed),
            _ => out.push(Statement {
                line: idx + 1,
                text: trimmed.to_string(),
            }),
        }
    }

    out
}

/// Which pool a statement checks a connection out of, if it checks one out.
fn checkout_pool(text: &str) -> Option<&'static str> {
    if text.starts_with("//") || text.starts_with('*') {
        return None;
    }

    if text.contains("pool.write()") {
        return Some("write");
    }

    if text.contains("pool.get()") {
        return Some("read");
    }

    None
}

/// `DbPool::write` is where a connection that will open `BEGIN IMMEDIATE`
/// comes from.
///
/// A write transaction opened on a read connection holds that connection for
/// the whole write. Under `SQLite` WAL the read pool is sized for read
/// concurrency and the write pool is deliberately small, so a burst of writers
/// on read connections starves the readers the split exists to protect — and
/// the offending site reads exactly like a correct one, which is why this is
/// scanned rather than remembered.
#[test]
fn write_transactions_take_a_write_pool_connection() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let mut files = Vec::new();
    rs_files(&root.join("src"), &mut files);
    files.sort();

    let mut offenders = Vec::new();
    let mut paired = 0_usize;

    for file in files {
        let rel = relative(root, &file);

        let Ok(src) = fs::read_to_string(&file) else {
            continue;
        };

        let stmts = statements(&src);

        for (idx, stmt) in stmts.iter().enumerate() {
            if stmt.text.starts_with("//") || !stmt.text.contains("transaction_immediate(") {
                continue;
            }

            let window = &stmts[idx.saturating_sub(CHECKOUT_WINDOW)..idx];

            let Some(pool) = window.iter().rev().find_map(|s| checkout_pool(&s.text)) else {
                continue;
            };

            paired += 1;

            let allowed = READ_POOL_WRITE_TX_ALLOWLIST
                .iter()
                .any(|(path, _)| *path == rel);

            if pool == "read" && !allowed {
                offenders.push(format!("  {rel}:{}  {}", stmt.line, stmt.text));
            }
        }
    }

    assert!(
        paired > 0,
        "The scan paired no `transaction_immediate()` with a pool checkout. It looks for a \
         shape that no longer exists — update it in tests/chokepoint_copies.rs."
    );

    assert!(
        offenders.is_empty(),
        "A write transaction is opened on a READ-pool connection:\n{}\n\nTake the connection \
         from `pool.write()` instead — `DbPool::get` is the read pool, and holding one of its \
         connections for a write starves concurrent readers.\n\nIf a site genuinely must open \
         `BEGIN IMMEDIATE` on a read connection, add it to READ_POOL_WRITE_TX_ALLOWLIST in \
         tests/chokepoint_copies.rs with the reason.",
        offenders.join("\n")
    );
}

// ── autocommit writes come from the write pool too ───────────────────────────

/// How many statements after an `execute(` the scan looks for the SQL verb —
/// the statement text usually sits on the next line, as a `format!` argument.
const SQL_TEXT_WINDOW: usize = 4;

/// `(path, why an autocommit write on a read checkout there is acceptable)`.
const READ_POOL_AUTOCOMMIT_WRITE_ALLOWLIST: &[(&str, &str)] = &[];

/// The textual shapes of an autocommit write, compiled once per scan.
struct WriteShapes {
    /// A raw statement execution.
    execute: Regex,
    /// A write verb in SQL text.
    verb: Regex,
    /// The job-row writes that run as autocommit statements on the
    /// connection they are handed — the writes the scheduler issues outside
    /// any transaction of its own, and therefore the ones that were reaching
    /// the read pool.
    job_write: Regex,
}

impl WriteShapes {
    fn new() -> Self {
        let job_writes = [
            "update_heartbeat",
            "fail_job",
            "mark_stale",
            "complete_job",
            "complete_job_repairing",
            "insert_job",
            "insert_job_with",
            "set_job_data",
            "purge_old_jobs",
            "purge_old_jobs_for_slug",
            "cancel_pending_job",
            "cancel_pending_jobs",
            "delete_pending_failed_jobs_matching",
            "recover_stale_jobs",
            "strip_finished_payload",
        ]
        .join("|");

        Self {
            execute: Regex::new(r"\.execute(_batch)?\(").expect("execute pattern compiles"),
            verb: Regex::new(r"\b(UPDATE|INSERT|DELETE)\b").expect("verb pattern compiles"),
            job_write: Regex::new(&format!(r"\b({job_writes})\("))
                .expect("job-write pattern compiles"),
        }
    }

    /// Whether the statement at `idx` writes without a transaction of its
    /// own: a raw `execute` whose SQL text (this statement or the next few)
    /// carries a write verb, or one of the autocommit job-row writes.
    fn is_autocommit_write(&self, stmts: &[Statement], idx: usize) -> bool {
        let text = &stmts[idx].text;

        if self.execute.is_match(text) {
            let end = (idx + SQL_TEXT_WINDOW).min(stmts.len());

            return stmts[idx..end].iter().any(|s| self.verb.is_match(&s.text));
        }

        self.job_write.is_match(text)
    }
}

/// The scope a write statement runs in, from the nearest checkout or
/// transaction above it: `"tx"` (the other guard's domain), `"write"`, or
/// `"read"`. `None` when nothing in the window says — the connection arrived
/// as a parameter, and its caller's site is where it gets pinned.
fn write_scope(window: &[Statement]) -> Option<&'static str> {
    window.iter().rev().find_map(|s| {
        if s.text.starts_with("//") || s.text.starts_with('*') {
            return None;
        }

        if s.text.contains("transaction_immediate(") {
            return Some("tx");
        }

        checkout_pool(&s.text)
    })
}

/// Every statement in `src` that writes as autocommit on a read checkout.
fn autocommit_writes_on_read_checkouts(shapes: &WriteShapes, src: &str) -> Vec<Statement> {
    let stmts = statements(src);

    stmts
        .iter()
        .enumerate()
        .filter(|(idx, stmt)| {
            !stmt.text.starts_with("//")
                && shapes.is_autocommit_write(&stmts, *idx)
                && write_scope(&stmts[idx.saturating_sub(CHECKOUT_WINDOW)..*idx]) == Some("read")
        })
        .map(|(_, stmt)| Statement {
            line: stmt.line,
            text: stmt.text.clone(),
        })
        .collect()
}

/// A write that opens no transaction still takes a write-pool connection.
///
/// The transaction guard above keys on `transaction_immediate()`, so it never
/// saw an autocommit `UPDATE` on a `pool.get()` connection — the scheduler's
/// heartbeats ran that way, and a heartbeat blocked behind a long write on a
/// read connection was one half of a job being executed twice. The same
/// starvation argument applies: the read pool is sized for readers, and a
/// writer sitting on one of its connections while it waits for the write lock
/// takes that connection from them.
#[test]
fn autocommit_writes_take_a_write_pool_connection() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    let mut files = Vec::new();
    rs_files(&root.join("src"), &mut files);
    files.sort();

    let shapes = WriteShapes::new();
    let mut offenders = Vec::new();
    let mut allowlisted_hits = 0_usize;

    for file in files {
        let rel = relative(root, &file);

        let Ok(src) = fs::read_to_string(&file) else {
            continue;
        };

        let allowed = READ_POOL_AUTOCOMMIT_WRITE_ALLOWLIST
            .iter()
            .any(|(path, _)| *path == rel);

        for stmt in autocommit_writes_on_read_checkouts(&shapes, &src) {
            if allowed {
                allowlisted_hits += 1;
            } else {
                offenders.push(format!("  {rel}:{}  {}", stmt.line, stmt.text));
            }
        }
    }

    assert!(
        allowlisted_hits > 0 || READ_POOL_AUTOCOMMIT_WRITE_ALLOWLIST.is_empty(),
        "No allowlisted file matches the autocommit-write scan any more — remove the stale \
         rows from READ_POOL_AUTOCOMMIT_WRITE_ALLOWLIST in tests/chokepoint_copies.rs."
    );

    assert!(
        offenders.is_empty(),
        "An autocommit write runs on a READ-pool connection:\n{}\n\nTake the connection from \
         `pool.write()` instead — `DbPool::get` is the read pool, and a writer waiting for the \
         write lock on one of its connections starves concurrent readers.\n\nIf a site \
         genuinely must write on a read connection, add it to \
         READ_POOL_AUTOCOMMIT_WRITE_ALLOWLIST in tests/chokepoint_copies.rs with the reason.",
        offenders.join("\n")
    );
}

/// Positive control: the scan fires on the shape it exists for — a job-row
/// write on a `pool.get()` checkout, and a raw `execute` whose UPDATE sits on
/// the following line — and stays quiet once the checkout is `pool.write()`
/// or a transaction is open (the other guard's domain).
#[test]
fn the_autocommit_write_scan_flags_a_read_checkout() {
    let job_write = "fn beat(pool: &DbPool) {\n    let conn = pool.get().unwrap();\n    \
                     job_query::update_heartbeat(&conn, id).unwrap();\n}\n";
    let raw_write = "fn stamp(pool: &DbPool) {\n    let conn = pool.get().unwrap();\n    \
                     conn.execute(\n        &format!(\"UPDATE t SET x = 1 WHERE id = {p}\"),\n        \
                     &[],\n    ).unwrap();\n}\n";

    let shapes = WriteShapes::new();

    let hits = autocommit_writes_on_read_checkouts(&shapes, job_write);
    let texts: Vec<&str> = hits.iter().map(|h| h.text.as_str()).collect();
    assert_eq!(hits.len(), 1, "{texts:?}");
    assert_eq!(hits[0].line, 3);

    let hits = autocommit_writes_on_read_checkouts(&shapes, raw_write);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].line, 3);

    let on_write_pool = job_write.replace("pool.get()", "pool.write()");
    assert!(autocommit_writes_on_read_checkouts(&shapes, &on_write_pool).is_empty());

    let in_transaction = job_write.replace(
        "let conn = pool.get().unwrap();",
        "let mut conn = pool.get().unwrap();\n    let tx = conn.transaction_immediate().unwrap();",
    );
    assert!(
        autocommit_writes_on_read_checkouts(&shapes, &in_transaction).is_empty(),
        "a write inside a transaction is the transaction guard's to flag"
    );

    let read_only = "fn count(pool: &DbPool) {\n    let conn = pool.get().unwrap();\n    \
                     conn.execute(\"SELECT 1\", &[]).unwrap();\n}\n";
    assert!(autocommit_writes_on_read_checkouts(&shapes, read_only).is_empty());
}

/// A gated helper in the middle of a file must hide only itself. The scan
/// used to stop at the first test gate it met, which hid every production
/// line after it — thousands in the Postgres backend.
#[test]
fn production_code_keeps_the_code_after_a_gated_helper() {
    let src = "fn a() {}\n#[cfg(test)]\nfn helper() {\n    let x = \"{\";\n}\nfn b() {}\n\
               #[cfg(not(test))]\nfn c() {}\n#[cfg(all(test, feature = \"sqlite\"))]\n\
               mod tests {\n    fn t() {}\n}\n";

    let kept = production_code(src);

    assert!(kept.contains("fn a()") && kept.contains("fn b()"), "{kept}");
    assert!(
        kept.contains("fn c()"),
        "not(test) is production code: {kept}"
    );
    assert!(
        !kept.contains("helper") && !kept.contains("fn t()"),
        "{kept}"
    );
    assert_eq!(
        kept.lines().count(),
        src.lines().count(),
        "line numbers must be preserved"
    );
}

/// Regression: the gate evaluator assumed every non-`test` atom is on, so a
/// negated atom — `#[cfg(not(tarpaulin_include))]`, which marks most CLI
/// entry points and `main.rs`, or `not(feature = "x")` — read as test-only and
/// its whole item was blanked. Every guard built on this scan was blind to
/// those functions. An item is test-only exactly when no build with `test`
/// off compiles it.
#[test]
fn production_code_keeps_items_gated_on_a_negated_atom() {
    let src = "#[cfg(not(tarpaulin_include))]\nfn entry() {}\n\
               #[cfg(not(feature = \"postgres\"))]\nfn sqlite_only() {}\n\
               #[cfg(all(not(tarpaulin_include), feature = \"sqlite\"))]\nfn both() {}\n\
               #[cfg(all(test, not(tarpaulin_include)))]\nfn gated_test() {}\n\
               #[cfg(not(any(not(test), feature = \"x\")))]\nfn never_in_prod() {}\n";

    let kept = production_code(src);

    assert!(kept.contains("fn entry()"), "{kept}");
    assert!(kept.contains("fn sqlite_only()"), "{kept}");
    assert!(kept.contains("fn both()"), "{kept}");
    assert!(!kept.contains("fn gated_test()"), "{kept}");
    assert!(
        !kept.contains("fn never_in_prod()"),
        "not(any(not(test), …)) needs test on: {kept}"
    );
}

/// Positive control against the real tree: the Postgres backend has a test
/// gate near its top and its `DbConnection` impl far below it.
#[test]
fn production_code_reaches_the_postgres_connection_impl() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/db/backend/postgres.rs");
    let src = fs::read_to_string(path).expect("postgres backend source");

    let kept = production_code(&src);

    assert!(
        kept.contains("impl DbConnection for PgConnection"),
        "the scan stopped before the connection impl"
    );
    assert!(
        !kept.contains("mod tests"),
        "the trailing test module must be blanked"
    );
}
