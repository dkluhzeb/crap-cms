//! Architectural guard for **surface parity**.
//!
//! The admin (HTTP), gRPC, Lua-CRUD, and MCP surfaces must all route document
//! CRUD through the unified service layer (`service::*`). The service op owns
//! the cross-cutting behavior — access checks, validation, write hooks,
//! reference counting, draft visibility, and read/write field stripping. A
//! surface that reaches *past* the service layer into `db::ops::*` or the
//! `query::*` primitives silently re-implements (or skips) that behavior, and
//! the surfaces drift apart. That class of bug has bitten this project before.
//!
//! This test fails when a surface handler calls a service-bypass primitive
//! that isn't in the reviewed [`ALLOWLIST`]. To make it pass you either:
//!   1. route the call through the matching `service::*` op (the default), or
//!   2. if the bypass is genuinely intended, add it to `ALLOWLIST` with a
//!      one-line justification — which forces the decision through review.
//!
//! Writes (`query::create/update/delete/...`) are intentionally **not**
//! allowlisted anywhere: a surface must never write outside the service layer.
//!
//! **Scope & limits of these guards.** The scans are textual (per-line
//! substring / whole-file `contains`), not AST-based, so they catch the common
//! case but not every evasion. The bypass scan matches a primitive in both
//! shapes it can appear in — module-qualified (`query::find(`) and, because the
//! house style imports names directly, as a bare call in a file whose `use`
//! statements bind that name from the bypass module (`use crate::db::ops::{a,
//! b};` then `a(...)`). What still slips past: a call split across lines, an
//! aliased import (`use ... as foo; foo(...)`), a glob (`use ...::*`), and a
//! re-export under a different name. The scans also only cover
//! [`SURFACE_ROOTS`] — request-handling surfaces — so background workers (the
//! scheduler, cron tasks) that legitimately call `query::*` directly are out of
//! scope by design. Treat these as a high-signal tripwire for the obvious
//! regression, not a proof of total coverage.

use std::{
    fs,
    mem::take,
    path::{Path, PathBuf},
};

mod common;

use common::production_code;

/// Surface roots whose handlers must delegate document CRUD to the service
/// layer, each paired with the minimum number of `.rs` files the scan must find
/// under it. Without that floor, renaming or moving a root leaves the scans
/// walking an empty directory and every guard over that surface passes
/// vacuously. The floors sit well below the real counts so ordinary refactors
/// don't trip them.
const SURFACE_ROOTS: &[(&str, usize)] = &[
    ("src/admin/handlers", 60),
    ("src/api/handlers", 30),
    ("src/api/upload", 3),
    ("src/mcp/tools", 20),
    ("src/hooks/lua_api/crud", 20),
];

/// Inventory floor for `src/commands`, for the same reason.
const CLI_FILE_FLOOR: usize = 40;

/// Service-bypass call forms, spelled `module::fn_name(`. Reads skip the read
/// lifecycle (access/draft/stripping/hooks); writes skip
/// validation/hooks/ref-counting.
///
/// The spelling is the entry's identity — [`ALLOWLIST`] and
/// [`CLI_WRITE_ALLOWLIST`] key off it — but matching normalizes each entry to
/// its `(module, fn name)` pair so both the qualified call and the imported
/// bare-name call resolve to the same entry, and one allowlist row suppresses
/// either shape.
const FORBIDDEN_CALLS: &[&str] = &[
    // Pool-based raw reads — these exist *only* as a service bypass.
    "ops::find_documents(",
    "ops::find_document_by_id(",
    "ops::count_documents(",
    "ops::get_global(",
    // Query-layer document reads.
    "query::find(",
    "query::find_by_id(",
    "query::count(",
    "query::get_global(",
    // Query-layer writes — must NEVER appear in a surface (no allowlist entry
    // is permitted for these).
    "query::create(",
    "query::update(",
    "query::update_partial(",
    "query::update_global(",
    "query::delete(",
    "query::soft_delete(",
    "query::restore(",
    "query::create_version(",
    // Account-state and credential writes — each has a service op that also
    // bumps the session version and tears down the user's live streams
    // (`service::auth::lock_user`, `set_password`, …). A surface writing them
    // raw would leave a revoked user's sessions and streams running.
    "query::lock_user(",
    "query::unlock_user(",
    "query::mark_verified(",
    "query::mark_unverified(",
    "query::bump_session_version(",
    "query::update_password(",
    "query::reset_totp(",
    // Raw snapshot / draft-overlay read primitives — these skip the
    // view-scope/access path entirely and exist only inside `service::read`.
    // A surface calling them would bypass draft/trash/versions gating.
    "ops::find_by_id_full(",
    "ops::snapshot_read_document(",
];

/// Reviewed, intentional exceptions: `(path suffix, call form)`. Every entry is
/// a read; a write must never be added here.
const ALLOWLIST: &[(&str, &str)] = &[
    // ── Field-context enrichment (relationship/join/upload display labels) ──
    // The enrichment label reads are ACCESS-GATED here: `gated_find_by_id` /
    // `gated_find` AND the target collection's published∪draft view filter
    // (`resolve_view_scope`, downgraded to the viewer's access) into the query
    // before reading — so a viewer never sees the label, id, or count of a
    // target they cannot read. This is a deliberate lightweight gated read
    // (labels only), kept out of the populating `service::find_documents` path.
    (
        "admin/handlers/field_context/enrich/gated.rs",
        "query::find(",
    ),
    // The `me` endpoint reads the authenticated user's own record.
    ("api/handlers/auth/me.rs", "query::find_by_id("),
];

fn is_allowlisted(rel_path: &str, call: &str) -> bool {
    ALLOWLIST
        .iter()
        .any(|(suffix, allowed_call)| rel_path.ends_with(suffix) && *allowed_call == call)
}

/// Collect `.rs` files under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The `.rs` files under one surface root, with the root's inventory floor
/// enforced so a moved or renamed directory fails loudly instead of quietly
/// scanning nothing.
fn surface_files(root: &Path, surface: &str, floor: usize) -> Vec<PathBuf> {
    let dir = root.join(surface);
    let mut files = Vec::new();
    rust_files(&dir, &mut files);

    assert!(
        files.len() >= floor,
        "surface root `{surface}` yielded {} .rs file(s), below the floor of \
         {floor} — the directory was moved, renamed, or emptied, and every scan \
         over it is now vacuous. Point SURFACE_ROOTS at the new location (and \
         re-check the floor).",
        files.len()
    );

    files
}

/// Path of `file` relative to the crate root, with forward slashes.
fn relative_path(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Split a `module::fn_name(` entry into its module and bare fn name — the
/// normalized key both match forms and both allowlists agree on.
fn split_call(entry: &str) -> (&str, &str) {
    let body = entry.trim_end_matches('(');
    body.split_once("::").unwrap_or(("", body))
}

/// Every occurrence of `entries` in `contents`, as `(1-based line, entry)`.
///
/// A primitive counts as reached either module-qualified (`query::find(`) or by
/// bare name, when the file's `use` statements bind that name from the bypass
/// module — the shape the short-import house style produces.
fn hits_for<'a>(contents: &str, entries: &[&'a str]) -> Vec<(usize, &'a str)> {
    // Comments and test code are scrubbed line for line, so a doc line naming
    // a primitive is not a call to it and reported lines stay real.
    let code_only = production_code(contents);
    let bare = bare_visible(&code_only, entries);
    let mut hits = Vec::new();

    for (idx, code) in code_only.lines().enumerate() {
        for entry in entries {
            let (_, name) = split_call(entry);
            let reached = code.contains(entry) || (bare.contains(entry) && calls_bare(code, name));

            if reached {
                hits.push((idx + 1, *entry));
            }
        }
    }

    hits
}

/// The entries a file can reach by bare name, because it imports them from the
/// bypass module they belong to.
fn bare_visible<'a>(contents: &str, entries: &[&'a str]) -> Vec<&'a str> {
    let statements = use_statements(contents);

    entries
        .iter()
        .copied()
        .filter(|entry| {
            let (module, name) = split_call(entry);
            statements
                .iter()
                .any(|stmt| binds_from_module(stmt, module, name))
        })
        .collect()
}

/// Every `use` statement in `contents`, whitespace removed, so a tree-style
/// import spanning many lines becomes one flat path expression.
fn use_statements(contents: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut in_use = false;

    for line in contents.lines() {
        let trimmed = line.trim();

        if !in_use {
            if !strip_visibility(trimmed).starts_with("use ") {
                continue;
            }
            in_use = true;
        }

        current.extend(trimmed.chars().filter(|c| !c.is_whitespace()));

        if trimmed.ends_with(';') {
            statements.push(take(&mut current));
            in_use = false;
        }
    }

    statements
}

/// Strip a leading `pub` / `pub(crate)` / `pub(in path)` visibility modifier.
fn strip_visibility(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("pub") else {
        return text;
    };

    let rest = rest.strip_prefix('(').map_or(rest, |inner| {
        inner.split_once(')').map_or(rest, |(_, after)| after)
    });

    rest.trim_start()
}

/// Whether one whitespace-stripped `use` statement binds `name` as a bare
/// identifier coming from `module`.
fn binds_from_module(stmt: &str, module: &str, name: &str) -> bool {
    let anchor = format!("{module}::");
    let mut rest = stmt;

    while let Some(pos) = rest.find(&anchor) {
        // The anchor must be a whole path segment: `sub_ops::` is not `ops::`.
        let is_segment = rest[..pos]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        let after = &rest[pos + anchor.len()..];

        if is_segment && binds_name(after, name) {
            return true;
        }

        rest = after;
    }

    false
}

/// Whether the path tail right after `module::` binds `name` — directly, or as
/// a top-level item of a `{…}` group.
fn binds_name(after: &str, name: &str) -> bool {
    let Some(inner) = brace_group(after) else {
        return leading_ident(after) == name;
    };

    split_top_level(inner)
        .iter()
        .any(|item| leading_ident(item) == name)
}

/// The contents of the balanced `{…}` group `text` opens with, if it opens one.
fn brace_group(text: &str) -> Option<&str> {
    let body = text.strip_prefix('{')?;
    let mut depth = 1usize;

    for (idx, ch) in body.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&body[..idx]);
                }
            }
            _ => {}
        }
    }

    None
}

/// Split a brace group's contents on its top-level commas.
fn split_top_level(inner: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;

    for (idx, ch) in inner.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&inner[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }
    parts.push(&inner[start..]);

    parts
}

/// The leading identifier of `text`.
fn leading_ident(text: &str) -> &str {
    let end = text
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(text.len());
    &text[..end]
}

/// Whether `code` calls the bare identifier `name` — a standalone `name(`, not
/// a method call (`.name(`) or a differently-qualified path (`other::name(`).
fn calls_bare(code: &str, name: &str) -> bool {
    let needle = format!("{name}(");
    let mut from = 0usize;

    while let Some(pos) = code[from..].find(&needle) {
        let at = from + pos;
        let preceded_by_path = code[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == ':');

        if !preceded_by_path {
            return true;
        }

        from = at + needle.len();
    }

    false
}

#[test]
fn surfaces_do_not_bypass_the_service_layer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations: Vec<String> = Vec::new();

    for (surface, floor) in SURFACE_ROOTS {
        for file in surface_files(root, surface, *floor) {
            let contents = fs::read_to_string(&file).unwrap_or_default();
            let rel = relative_path(root, &file);

            violations.extend(
                hits_for(&contents, FORBIDDEN_CALLS)
                    .into_iter()
                    .filter(|(_, call)| !is_allowlisted(&rel, call))
                    .map(|(lineno, call)| format!("  {rel}:{lineno}  {call}")),
            );
        }
    }

    assert!(
        violations.is_empty(),
        "Surface handler(s) bypass the service layer (parity risk).\n\
         Route the call through the matching `service::*` op, or — if the \
         bypass is genuinely intended — add it to ALLOWLIST in \
         tests/surface_parity.rs with a justification.\n\n{}",
        violations.join("\n")
    );
}

/// Access-changing write ops: editing/restoring/deleting one of these can change
/// a user's access, so the handler must let the service tear down that user's
/// live-update streams. `create` is excluded (a new document has no pre-existing
/// stream to invalidate). Also includes the auth state-change ops that revoke a
/// privilege and call `publish_user_invalidation` directly (`consume_reset_token`).
/// `lock_user`/`mark_unverified` are passed as fn-pointers (via
/// `account_action_blocking`) so they don't textually match a `(` form here —
/// they're guarded structurally by `auth_revoking_handlers_request_invalidation`
/// below instead.
const INVALIDATION_WRITE_OPS: &[&str] = &[
    // Direct service calls (pre-op-core style; admin restore_action still).
    "update_document(",
    "update_many(",
    "unpublish_document(",
    "undelete_document(",
    "restore_collection_version(",
    "delete_document(",
    "delete_many(",
    "consume_reset_token(",
    // Operation-core bodies — post-migration, codecs invoke these instead of
    // the service fns; without them this guard matched nothing and was
    // vacuous. (`op::run`/`run_blocking` dispatchers attach infra themselves,
    // so only direct `<Op>::run(` calls need scanning.)
    "Update::run(",
    "UpdateMany::run(",
    "Unpublish::run(",
    "Undelete::run(",
    "RestoreVersion::run(",
    "Delete::run(",
    "DeleteMany::run(",
];

/// Architectural guard: any surface handler that builds a `ServiceContext` and
/// performs an access-changing write MUST attach `invalidation_transport`.
/// Otherwise the service's post-commit `invalidate_user_streams_if_auth` is a
/// silent no-op and a role/group change leaves the user's live streams running
/// on stale access — exactly the bug that hid in the unpublish handlers on three
/// surfaces. (The service orchestrators are intentionally NOT scanned: they build
/// an inner context without the transport because the outer surface context owns
/// the post-commit publish.)
///
/// `.infra(...)` counts as attaching it: the `AppInfra` bundle carries the
/// invalidation transport and `ServiceContext::infra` sets it unconditionally, so
/// a handler that builds its context via `.infra(...)` cannot forget the transport
/// (a strictly stronger guarantee than the explicit `.invalidation_transport(...)`
/// call this guard originally looked for).
#[test]
fn write_surfaces_attach_invalidation_transport() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders: Vec<String> = Vec::new();

    for (surface, floor) in SURFACE_ROOTS {
        for file in surface_files(root, surface, *floor) {
            let contents = fs::read_to_string(&file).unwrap_or_default();

            if misses_invalidation_transport(&contents) {
                offenders.push(format!("  {}", relative_path(root, &file)));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "Write-path surface handler(s) build a ServiceContext for an \
         access-changing write but never attach the invalidation transport \
         (neither `.invalidation_transport(...)` nor `.infra(...)`), so \
         live-stream teardown silently no-ops on a role change. Attach it — \
         either explicitly from the surface's invalidation transport, or via \
         `.infra(...)` (which bundles it) — like the sibling \
         update/undelete/restore handlers do.\n\n{}",
        offenders.join("\n")
    );
}

/// The decision core of [`write_surfaces_attach_invalidation_transport`],
/// extracted so the positive control below can prove it still fires.
fn misses_invalidation_transport(contents: &str) -> bool {
    let builds_ctx = contents.contains("ServiceContext::collection");
    let does_write = INVALIDATION_WRITE_OPS
        .iter()
        .any(|op| contents.contains(op));
    let attaches_transport =
        contents.contains("invalidation_transport") || contents.contains(".infra(");

    builds_ctx && does_write && !attaches_transport
}

/// Positive control: the invalidation matcher must
/// fire on a synthetic violating handler. This exact guard was once
/// vacuous — after the op-core migration it matched only retired service
/// fn names and flagged nothing.
#[test]
fn invalidation_scan_fires_on_synthetic_violation() {
    let synthetic = r"
        let ctx = ServiceContext::collection(slug, &def).conn(conn).build();
        Delete::run(&ctx, input)?;
    ";
    assert!(
        misses_invalidation_transport(synthetic),
        "a ServiceContext + write op with no transport must be flagged"
    );

    let compliant = r"
        let ctx = ServiceContext::collection(slug, &def).infra(&state.infra).build();
        Delete::run(&ctx, input)?;
    ";
    assert!(
        !misses_invalidation_transport(compliant),
        ".infra() bundles the transport and must pass"
    );
}

/// Liveness check for the matcher's vocabulary:
/// every `INVALIDATION_WRITE_OPS` name must still occur in the codebase
/// (service layer, op bodies, or the surfaces themselves). This is the
/// exact decay mode that made the guard vacuous once — the op-core
/// migration renamed the write entry points and the old list matched
/// nothing.
#[test]
fn invalidation_write_ops_vocabulary_is_live() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut sources = String::new();
    for dir in [
        "src/service",
        "src/admin",
        "src/api",
        "src/mcp",
        "src/hooks",
    ] {
        let mut files = Vec::new();
        rust_files(&root.join(dir), &mut files);
        for f in files {
            sources.push_str(&fs::read_to_string(f).unwrap_or_default());
        }
    }

    let stale: Vec<&&str> = INVALIDATION_WRITE_OPS
        .iter()
        .filter(|op| !sources.contains(**op))
        .collect();

    assert!(
        stale.is_empty(),
        "INVALIDATION_WRITE_OPS entries match nothing in the codebase — \
         the guard is going vacuous again; update the vocabulary: {stale:?}"
    );
}

/// Structural guard for the auth state-change handlers: the session-invalidation
/// flag is now **derived from the `AccountAction`** itself
/// (`AccountAction::invalidates_sessions()`), not passed as a hand-written bool
/// per handler — so a *revoking* action (`Lock`, `Unverify`) can no longer ship
/// paired with the wrong flag (the `unverify` regression that once left an
/// already-connected SSE/subscribe stream running on a revoked session). The
/// derivation truth table is unit-tested in `account.rs`
/// (`account_action_flags_match_the_action`); here we guard that the handlers
/// keep routing their action through `account_action_input` (where the
/// derivation lives) and don't reintroduce a per-handler bool.
#[test]
fn auth_invalidation_is_derived_from_the_action() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = root.join("src/api/handlers/auth/account.rs");
    let contents = fs::read_to_string(&path).expect("account.rs must exist");

    // One source: the input builder attaches the invalidation transport iff the
    // action says so, instead of each handler passing a literal bool.
    assert!(
        contents.contains("invalidates_sessions()"),
        "account_action_input must derive the invalidation transport from the \
         action (AccountAction::invalidates_sessions()), not a per-handler bool"
    );

    // Every handler that dispatches an account action must build its input via
    // `account_action_input(.., action)`, so the derivation applies to it.
    let actions = [
        "AccountAction::Lock",
        "AccountAction::Unlock",
        "AccountAction::Verify",
        "AccountAction::Unverify",
    ];
    let mut offenders: Vec<String> = Vec::new();

    for chunk in contents.split("async fn ").skip(1) {
        let handler = chunk.split('(').next().unwrap_or("").trim();

        for action in actions {
            // A handler references the action only in its dispatch body once it
            // has bound `let action = AccountAction::X;`; that same `action` must
            // reach `account_action_input`.
            if chunk.contains(&format!("let action = {action};"))
                && !chunk.contains("account_action_input(token, headers, &req, action)")
            {
                offenders.push(format!("  {handler} (dispatches {action})"));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "Auth handler(s) dispatch an account action without routing that action \
         through `account_action_input`, bypassing the derived invalidation \
         flag.\n\n{}",
        offenders.join("\n")
    );
}

/// The chokepoints through which a privilege revocation retires the user's
/// already-issued credentials. Each one bumps `_session_version` (or is the
/// bump itself) and, with a transport attached, tears down the user's open
/// live streams — so a handler that reaches one of them cannot leave a twin
/// credential valid.
const REVOCATION_CHOKEPOINTS: &[&str] = &[
    "bump_session_version(",
    "perform_account_action(",
    "consume_reset_token(",
];

/// Source markers that identify a handler which ends a session, locks or
/// unlocks an account, or changes a password. Matched against production code
/// only (comments and test modules blanked), so a doc mention doesn't count.
const REVOKING_MARKERS: &[&str] = &[
    "fn logout",
    "AccountAction::",
    "perform_account_action(",
    "LockUpdate::",
    "consume_reset_token(",
    // The bare account primitives: a handler calling one of these directly
    // has skipped the authorizing chokepoint.
    "lock_user(",
    "unlock_user(",
    "update_password(",
];

/// The reviewed inventory of session-revoking surface handlers. The scan must
/// find exactly these: a new revoking handler has to be added here (forcing
/// the chokepoint check through review), and a vanished one is noticed too.
const REVOKING_HANDLERS: &[&str] = &[
    "src/admin/handlers/auth/logout_action.rs",
    "src/admin/handlers/auth/reset_password_action.rs",
    "src/admin/handlers/collections/shared/update.rs",
    "src/api/handlers/auth/account.rs",
    "src/api/handlers/auth/reset_password.rs",
];

/// Whether `production` (already scrubbed) is a revoking handler body.
fn is_revoking_handler(production: &str) -> bool {
    REVOKING_MARKERS.iter().any(|m| production.contains(m))
}

/// The decision core of [`auth_revoking_handlers_request_invalidation`]: a
/// revoking handler body that reaches none of the chokepoints.
fn revokes_without_chokepoint(production: &str) -> bool {
    is_revoking_handler(production)
        && !REVOCATION_CHOKEPOINTS
            .iter()
            .any(|call| production.contains(call))
}

/// Structural guard: every surface handler that ends a session, locks a user,
/// or changes a password must reach a revocation chokepoint. The admin logout
/// once read its principal from an extension no middleware had inserted on its
/// route — the bump silently never ran and a captured JWT stayed valid until
/// `exp`, with the cookie-clearing test still green.
#[test]
fn auth_revoking_handlers_request_invalidation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut found: Vec<String> = Vec::new();
    let mut offenders: Vec<String> = Vec::new();

    for (surface, floor) in SURFACE_ROOTS {
        for file in surface_files(root, surface, *floor) {
            let production = production_code(&fs::read_to_string(&file).unwrap_or_default());
            let rel = relative_path(root, &file);

            if is_revoking_handler(&production) {
                found.push(rel.clone());
            }
            if revokes_without_chokepoint(&production) {
                offenders.push(format!("  {rel}"));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "Session-revoking handler(s) never reach a revocation chokepoint \
         ({REVOCATION_CHOKEPOINTS:?}), so the credentials already issued to \
         the affected user stay valid.\n\n{}",
        offenders.join("\n")
    );

    found.sort_unstable();
    let mut expected: Vec<String> = REVOKING_HANDLERS.iter().map(|s| (*s).to_string()).collect();
    expected.sort_unstable();
    assert_eq!(
        found, expected,
        "the set of session-revoking handlers changed — review the new or \
         missing handler against the chokepoints and update REVOKING_HANDLERS"
    );
}

/// Positive control: the revocation matcher must fire on a handler that
/// locks via the bare primitive (skipping the authorizing chokepoint) and
/// on a logout that never bumps, and must pass the compliant shapes.
#[test]
fn revocation_scan_fires_on_synthetic_violation() {
    let bare_lock = "let action = AccountAction::Lock;\nlock_user(&ctx, &id)?;\n";
    assert!(
        revokes_without_chokepoint(bare_lock),
        "a lock through the bare primitive must be flagged"
    );

    let cookie_only_logout = "pub async fn logout_action() -> Response {\n\
                              clear_session_cookies(dev_mode, same_site)\n}\n";
    assert!(
        revokes_without_chokepoint(cookie_only_logout),
        "a logout that only clears cookies must be flagged"
    );

    let compliant = "let action = AccountAction::Lock;\n\
                     perform_account_action(&ctx, &id, action)?;\n";
    assert!(!revokes_without_chokepoint(compliant));

    let unrelated = "let doc = Update::run(&ctx, args)?;\n";
    assert!(!is_revoking_handler(unrelated));
}

/// Reviewed offline-admin CLI write paths: `(path suffix, write call)`.
/// Every entry documents which invariants the site maintains by hand.
const CLI_WRITE_ALLOWLIST: &[(&str, &str)] = &[
    // `user reset-totp`: TOTP-mode check + destructive confirm; clears the
    // three `_totp_*` system columns in one atomic UPDATE. No session revoke
    // or stream teardown: resetting the second factor grants nothing and
    // revokes nothing until the next login re-enrolls. Outside FTS/ref-count
    // scope by design (system columns only).
    ("commands/user/modify.rs", "query::reset_totp("),
    // `db cleanup --confirm`: deletes junction rows of locales the project no
    // longer configures, inside the cleanup's one transaction, which then
    // recomputes every reference count (`migrate::recompute_ref_counts`) —
    // the rows can hold references. No hooks or events: the rows are
    // unreachable by every read.
    (
        "commands/db/cleanup/apply.rs",
        "query::delete_rows_outside_locales(",
    ),
    // `import`: a raw restore. Rebuilds join rows under their exported ids
    // inside the import's one transaction; reference counts are settled once
    // every document exists, FTS is re-indexed per document, and after the
    // commit the cache is cleared and overwritten accounts' streams are torn
    // down. The parent row is upserted with hand-built SQL + `tx.execute`,
    // and replaced sessions are revoked through `query::auth::…` — both
    // invisible to this textual primitive scan, per the scan limits
    // documented at the top of this file.
    (
        "commands/export/import_write.rs",
        "query::restore_join_table_data(",
    ),
];

/// CLI write primitives are confined to reviewed offline-admin paths.
///
/// A CLI (or any non-surface) path that mutates documents or accounts with
/// raw `query::*` writes silently bypasses the service layer's invariants —
/// validation, hooks, ref counting, FTS sync, delete protection, session
/// revocation and stream teardown. The reviewed paths above hand-replicate
/// exactly the invariants they need (and regression tests pin them:
/// `import_adjusts_ref_counts`,
/// `applying_the_cleanup_recounts_the_deleted_rows_references`). A new
/// write call anywhere else in `src/commands` must either route through
/// a service op or be added here with a justification — which forces
/// the decision through review.
#[test]
fn cli_commands_write_only_through_reviewed_paths() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src/commands"), &mut files);

    assert!(
        files.len() >= CLI_FILE_FLOOR,
        "src/commands yielded {} .rs file(s), below the floor of {CLI_FILE_FLOOR} \
         — the directory was moved or emptied and this scan is now vacuous",
        files.len()
    );

    let mut violations = Vec::new();
    for file in &files {
        let contents = fs::read_to_string(file).unwrap_or_default();
        let rel = relative_path(root, file);

        // Scans WRITE_PRIMITIVES rather than the write subset of
        // FORBIDDEN_CALLS: the join-row writes behind `db cleanup` and
        // `import` have no surface-bypass entry, so intersecting the two
        // lists would leave them unscanned here.
        violations.extend(
            hits_for(&contents, WRITE_PRIMITIVES)
                .into_iter()
                .filter(|(_, call)| !is_cli_allowlisted(&rel, call))
                .map(|(lineno, call)| format!("{rel}:{lineno} → {call}")),
        );
    }

    assert!(
        violations.is_empty(),
        "CLI write primitive outside the reviewed offline-admin paths — \
         route it through a service op, or add it to CLI_WRITE_ALLOWLIST \
         with the invariants it maintains by hand:\n{}",
        violations.join("\n")
    );
}

/// Positive control for the scan above: the
/// matcher must actually fire on a synthetic violation, so the guard
/// can never go silently vacuous the way the invalidation-transport
/// matcher once did.
#[test]
fn cli_write_scan_fires_on_synthetic_violation() {
    let qualified = "    query::delete(&tx, slug, id)?;\n";
    assert!(
        hits_for(qualified, WRITE_PRIMITIVES)
            .iter()
            .any(|(_, call)| *call == "query::delete("),
        "the write-primitive scan must match a module-qualified query::delete call"
    );

    // Same call reached through the short-import house style.
    let bare = "\
use crate::db::query::delete;

fn purge(tx: &Tx) {
    delete(tx, slug, id)?;
}
";
    assert!(
        hits_for(bare, WRITE_PRIMITIVES)
            .iter()
            .any(|(_, call)| *call == "query::delete("),
        "the write-primitive scan must match an imported bare-name delete call"
    );
}

/// The CLI allowlist itself must stay live: every entry's file must
/// still contain its call, or the entry is stale and gets removed
/// (mirrors `allowlist_has_no_stale_entries`).
#[test]
fn cli_write_allowlist_has_no_stale_entries() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    for (suffix, call) in CLI_WRITE_ALLOWLIST {
        let path = root.join("src").join(suffix);
        let present = fs::read_to_string(&path)
            .ok()
            .is_some_and(|c| c.contains(call));
        assert!(
            present,
            "stale CLI_WRITE_ALLOWLIST entry: {suffix} no longer contains {call}"
        );
    }
}

/// Whether `call` is a write primitive — one an allowlist may never permit.
fn is_write_primitive(call: &str) -> bool {
    WRITE_PRIMITIVES.contains(&call)
}

/// Whether `rel_path` is a reviewed offline-admin site for `call`.
fn is_cli_allowlisted(rel_path: &str, call: &str) -> bool {
    CLI_WRITE_ALLOWLIST
        .iter()
        .any(|(suffix, allowed_call)| rel_path.ends_with(suffix) && *allowed_call == call)
}

/// Document, join-row, account-state and credential write primitives the CLI
/// scan looks for. Each class joined once a CLI write of it went unseen: the
/// credential writes after the TOTP CLI shipped a raw one, the account-state
/// writes after `user lock` skipped the stream teardown its service op does,
/// and the raw join-row writes behind `db cleanup` and `import`.
const WRITE_PRIMITIVES: &[&str] = &[
    "query::create(",
    "query::update(",
    "query::update_partial(",
    "query::update_global(",
    "query::delete(",
    "query::soft_delete(",
    "query::restore(",
    "query::create_version(",
    "query::restore_join_table_data(",
    "query::delete_rows_outside_locales(",
    "query::lock_user(",
    "query::unlock_user(",
    "query::mark_verified(",
    "query::mark_unverified(",
    "query::bump_session_version(",
    "query::update_password(",
    "query::reset_totp(",
];

/// Whether `src` defines `name` as a callable `pub` / `pub(crate)` fn. A
/// private fn is unreachable from a surface, so naming one in a scan list
/// guards nothing.
fn defines_public_fn(src: &str, name: &str) -> bool {
    src.lines().any(|line| {
        let code = line.trim_start();
        if !code.starts_with("pub") {
            return false;
        }

        let rest = strip_visibility(code);
        let rest = rest.strip_prefix("async ").unwrap_or(rest);

        rest.starts_with(&format!("fn {name}(")) || rest.starts_with(&format!("fn {name}<"))
    })
}

/// The module source a scan entry's `module::` prefix names.
fn module_source<'a>(entry: &str, ops: &'a str, query: &'a str) -> &'a str {
    match split_call(entry).0 {
        "ops" => ops,
        "query" => query,
        other => panic!("scan entry `{entry}` names unknown bypass module `{other}`"),
    }
}

/// Anti-rot pin for the bypass vocabulary: every [`FORBIDDEN_CALLS`] entry must
/// still name a public fn in the module it claims. An entry whose fn was
/// renamed, made private, or deleted matches nothing and silently stops
/// guarding its primitive, while still reading like coverage.
#[test]
fn every_forbidden_call_still_names_a_live_fn() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ops = fs::read_to_string(root.join("src/db/ops.rs")).expect("src/db/ops.rs must exist");
    let query = concat_sources(root, "src/db/query");

    let dead: Vec<&&str> = FORBIDDEN_CALLS
        .iter()
        .filter(|entry| {
            let (_, name) = split_call(entry);
            !defines_public_fn(module_source(entry, &ops, &query), name)
        })
        .collect();

    assert!(
        dead.is_empty(),
        "FORBIDDEN_CALLS entries with no public `fn` of that name in their \
         module (src/db/ops.rs or src/db/query/**) — the bypass scan is \
         vacuous for them. Point the entry at the primitive that replaced it, \
         or drop it: {dead:?}"
    );
}

/// Vocabulary-liveness pin: every write-primitive
/// name must still exist in the query layer — a renamed primitive would
/// otherwise leave this scan matching nothing for that operation, the
/// exact decay that made the invalidation matcher vacuous once.
#[test]
fn write_primitive_vocabulary_is_live() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let query = concat_sources(root, "src/db/query");

    let stale: Vec<&&str> = WRITE_PRIMITIVES
        .iter()
        .filter(|call| !defines_public_fn(&query, split_call(call).1))
        .collect();

    assert!(
        stale.is_empty(),
        "WRITE_PRIMITIVES entries with no public `fn` in src/db/query — \
         the CLI scan is going vacuous for them: {stale:?}"
    );
}

#[test]
fn allowlist_has_no_stale_entries() {
    // Each allowlist entry must correspond to a real bypass call still present
    // in the named file. This proves the guard is non-vacuous and prevents a
    // stale entry from silently re-permitting a bypass after the original was
    // routed back through the service layer.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut stale: Vec<String> = Vec::new();

    for (suffix, call) in ALLOWLIST {
        // Allowlist suffixes are relative to `src/` (they omit the prefix so
        // the guard's `ends_with` match reads cleanly).
        let path = root.join("src").join(suffix);
        let present = fs::read_to_string(&path).ok().is_some_and(|c| {
            c.lines()
                .any(|l| !l.trim_start().starts_with("//") && l.contains(call))
        });

        if !present {
            stale.push(format!("  {suffix}  {call}"));
        }
    }

    assert!(
        stale.is_empty(),
        "ALLOWLIST has stale entries (no matching call found — remove them):\n{}",
        stale.join("\n")
    );
}

#[test]
fn allowlist_contains_no_write_primitives() {
    // Writes must always go through the service layer; an allowlist entry for a
    // write would defeat validation / hooks / ref-counting. Checked against the
    // one write-primitive list the CLI scan uses, so a primitive added there is
    // automatically barred from the surface allowlist too.
    for (path, call) in ALLOWLIST {
        assert!(
            !is_write_primitive(call),
            "ALLOWLIST entry for {path} permits a write primitive ({call}); \
             writes must never bypass the service layer"
        );
    }
}

/// Positive control for the bypass scan's bare-name arm: the short-import house
/// style (`use crate::db::ops::{…};` then a bare call) is the shape a bypass
/// actually takes in this codebase, and the qualified-only matcher never saw
/// it. The negative case is the one that makes the arm safe — the service layer
/// exports fns of the *same names*, so a bare call is only a bypass when the
/// file imported the name from the bypass module.
#[test]
fn bypass_scan_fires_on_the_bare_name_import_form() {
    let bypass = "\
use crate::db::{DbPool, ops::{count_documents, find_documents}};

fn list(pool: &DbPool) {
    let docs = find_documents(pool, \"posts\", def, &q, None).unwrap();
}
";
    assert!(
        hits_for(bypass, FORBIDDEN_CALLS)
            .iter()
            .any(|(_, call)| *call == "ops::find_documents("),
        "a bare call to a name imported from `db::ops` must be flagged"
    );

    let service = "\
use crate::service::{FindDocumentsInput, find_documents};

fn list(ctx: &ServiceContext) {
    let docs = find_documents(ctx, &input).unwrap();
}
";
    assert!(
        hits_for(service, FORBIDDEN_CALLS).is_empty(),
        "the service fn of the same name is the compliant path and must not be flagged"
    );
}

// ── Capability parity ───────────────────────────────────────────────────────
//
// The three *programmatic* surfaces — gRPC, Lua, MCP — must each expose the
// full canonical operation set. This catches the "operation available on some
// surfaces but not others" bug (e.g. `count` once missing from a surface).
// The admin HTTP surface is a UI, not a full CRUD API, so it is intentionally
// not part of this matrix.
//
// Each surface names the same operation differently, so the contract is an
// explicit op→marker matrix. Adding a new cross-surface op means adding a row
// here — which forces it to be wired on all three surfaces (or the test fails).

/// `(operation, lua marker, mcp marker, grpc marker)`.
/// - lua: the registered `crap.*` path (quoted so `find` ≠ `find_by_id`).
/// - mcp: the `exec_*` tool entrypoint definition.
/// - grpc: the `*_impl` RPC method definition.
const CANONICAL_OPS: &[(&str, &str, &str, &str)] = &[
    // Collection operations.
    (
        "find",
        "\"crap.collections.find\"",
        "fn exec_find(",
        "fn find_impl(",
    ),
    (
        "find_by_id",
        "\"crap.collections.find_by_id\"",
        "fn exec_find_by_id(",
        "fn find_by_id_impl(",
    ),
    (
        "count",
        "\"crap.collections.count\"",
        "fn exec_count(",
        "fn count_impl(",
    ),
    (
        "create",
        "\"crap.collections.create\"",
        "fn exec_create(",
        "fn create_impl(",
    ),
    (
        "update",
        "\"crap.collections.update\"",
        "fn exec_update(",
        "fn update_impl(",
    ),
    (
        "delete",
        "\"crap.collections.delete\"",
        "fn exec_delete(",
        "fn delete_impl(",
    ),
    (
        "create_many",
        "\"crap.collections.create_many\"",
        "fn exec_create_many(",
        "fn create_many_impl(",
    ),
    (
        "update_many",
        "\"crap.collections.update_many\"",
        "fn exec_update_many(",
        "fn update_many_impl(",
    ),
    (
        "delete_many",
        "\"crap.collections.delete_many\"",
        "fn exec_delete_many(",
        "fn delete_many_impl(",
    ),
    (
        "validate",
        "\"crap.collections.validate\"",
        "fn exec_validate(",
        "fn validate_impl(",
    ),
    (
        "undelete",
        "\"crap.collections.undelete\"",
        "fn exec_undelete(",
        "fn undelete_impl(",
    ),
    (
        "unpublish",
        "\"crap.collections.unpublish\"",
        "fn exec_unpublish(",
        "fn unpublish_impl(",
    ),
    (
        "list_versions",
        "\"crap.collections.list_versions\"",
        "fn exec_list_versions(",
        "fn list_versions_impl(",
    ),
    (
        "restore_version",
        "\"crap.collections.restore_version\"",
        "fn exec_restore_version(",
        "fn restore_version_impl(",
    ),
    // Global operations.
    (
        "global_get",
        "\"crap.globals.get\"",
        "fn exec_read_global(",
        "fn get_global_impl(",
    ),
    (
        "global_update",
        "\"crap.globals.update\"",
        "fn exec_update_global(",
        "fn update_global_impl(",
    ),
    (
        "global_validate",
        "\"crap.globals.validate\"",
        "fn exec_validate_global(",
        "fn validate_global_impl(",
    ),
];

/// Concatenate every `.rs` file under `dir` into one string.
fn concat_sources(root: &Path, dir: &str) -> String {
    let mut files = Vec::new();
    rust_files(&root.join(dir), &mut files);
    files
        .iter()
        .filter_map(|f| fs::read_to_string(f).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn all_canonical_ops_exist_on_every_programmatic_surface() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let lua = concat_sources(root, "src/hooks/lua_api");
    let mcp = concat_sources(root, "src/mcp/tools");
    let grpc = concat_sources(root, "src/api/handlers");

    let mut missing: Vec<String> = Vec::new();
    for (op, lua_m, mcp_m, grpc_m) in CANONICAL_OPS {
        if !lua.contains(lua_m) {
            missing.push(format!(
                "  op `{op}` missing from Lua surface (expected marker: {lua_m})"
            ));
        }
        if !mcp.contains(mcp_m) {
            missing.push(format!(
                "  op `{op}` missing from MCP surface (expected marker: {mcp_m})"
            ));
        }
        if !grpc.contains(grpc_m) {
            missing.push(format!(
                "  op `{op}` missing from gRPC surface (expected marker: {grpc_m})"
            ));
        }
    }

    assert!(
        missing.is_empty(),
        "Capability parity broken — an operation is exposed on some surfaces but not others.\n\
         Wire the operation on the missing surface, or (if the asymmetry is intended) \
         remove its row from CANONICAL_OPS in tests/surface_parity.rs with a note.\n\n{}",
        missing.join("\n")
    );
}

// ── Access-decision parity ──────────────────────────────────────────────────
//
// Every access decision must flow through the one shared evaluator
// (`HookRunner::check_access` → `service::auth`). CRUD ops check access *inside*
// the service layer; the event streams (gRPC Subscribe, admin SSE) resolve their
// per-view access through the shared `service::events::EventAccessMap` (also in
// the service layer); only a couple of non-CRUD surface ops (file upload/serve)
// have no service op and call the evaluator directly. This guard freezes that
// small set of surface-level `check_access` touchpoints: a new one fails CI,
// forcing review of whether it should instead go through a service op — and
// preventing a surface from growing its own ad-hoc access logic.

/// Surface files allowed to call `check_access` directly, each a reviewed
/// non-CRUD touchpoint that delegates to the shared evaluator.
const ACCESS_TOUCHPOINTS: &[&str] = &[
    // (The former gRPC `check_access_blocking` wrapper was deleted with the
    // operation-core port — bulk match-set gating now lives in the service
    // chokepoint `service::collections::bulk_access`, off-surface.)
    // Admin's shared access helpers (the admin-side centralization point).
    "admin/handlers/shared/access.rs",
    // REST upload helpers: file upload/serve is not a document-CRUD service op,
    // so the create/update/delete handlers gate via `check_upload_access` here,
    // which delegates to `hook_runner.check_access`.
    "api/upload/helpers.rs",
];

#[test]
fn surface_access_checks_are_frozen_to_reviewed_touchpoints() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations: Vec<String> = Vec::new();

    for (surface, floor) in SURFACE_ROOTS {
        for file in surface_files(root, surface, *floor) {
            let contents = fs::read_to_string(&file).unwrap_or_default();
            let rel = relative_path(root, &file);

            let touches_access = contents
                .lines()
                .any(|l| !l.trim_start().starts_with("//") && l.contains("check_access("));

            if touches_access && !ACCESS_TOUCHPOINTS.iter().any(|t| rel.ends_with(t)) {
                violations.push(format!("  {rel}"));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "New surface-level access check(s) found. Access decisions should flow through a \
         `service::*` op (which calls the shared evaluator); only reviewed non-CRUD touchpoints \
         call `check_access` directly. Route it through the service layer, or add the file to \
         ACCESS_TOUCHPOINTS in tests/surface_parity.rs with a justification.\n\n{}",
        violations.join("\n")
    );
}

#[test]
fn access_touchpoints_have_no_stale_entries() {
    // Each allowlisted touchpoint must still call `check_access` — keeps the
    // list honest and the freeze-guard non-vacuous.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut stale: Vec<String> = Vec::new();

    for tp in ACCESS_TOUCHPOINTS {
        let present = fs::read_to_string(root.join("src").join(tp))
            .ok()
            .is_some_and(|c| {
                c.lines()
                    .any(|l| !l.trim_start().starts_with("//") && l.contains("check_access("))
            });
        if !present {
            stale.push(format!("  {tp}"));
        }
    }

    assert!(
        stale.is_empty(),
        "ACCESS_TOUCHPOINTS has stale entries (no `check_access` call found — remove them):\n{}",
        stale.join("\n")
    );
}
