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

use std::fs;
use std::path::Path;

/// Surface roots whose handlers must delegate document CRUD to the service layer.
const SURFACE_ROOTS: &[&str] = &[
    "src/admin/handlers",
    "src/api/handlers",
    "src/mcp/tools",
    "src/hooks/lua_api/crud",
];

/// Service-bypass call forms. Reads skip the read lifecycle
/// (access/draft/stripping/hooks); writes skip validation/hooks/ref-counting.
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
    "query::undelete(",
    "query::create_version(",
];

/// Reviewed, intentional exceptions: `(path suffix, call form)`. Every entry is
/// a read; a write must never be added here.
const ALLOWLIST: &[(&str, &str)] = &[
    // ── Relationship-population layer ───────────────────────────────────────
    // Loads RELATED documents to render relationship-field picker labels and
    // previews. This is population, not a primary document read: it is
    // intentionally not per-collection access-gated (matches product
    // behavior) and is strictly read-only.
    (
        "admin/handlers/field_context/enrich/nested.rs",
        "query::find_by_id(",
    ),
    (
        "admin/handlers/field_context/enrich/types.rs",
        "query::find_by_id(",
    ),
    (
        "admin/handlers/field_context/enrich/types.rs",
        "query::find(",
    ),
    (
        "admin/handlers/field_context/enrich/enrichment.rs",
        "query::find_by_id(",
    ),
    // Upload-field rendering resolves the referenced upload document directly.
    (
        "admin/handlers/collections/shared/upload.rs",
        "query::find_by_id(",
    ),
    // The `me` endpoint reads the authenticated user's own record.
    ("api/handlers/auth/me.rs", "query::find_by_id("),
    // Versions-list page: raw read of the current document, used ONLY to
    // derive the page-heading title (`title_field`). The version history
    // itself is fetched through the service layer (`list_versions`). Traced:
    // the bypass exposes only the title field (the display label) and shows
    // the canonical/published title in the heading — not a read-stripping or
    // draft leak. Kept as-is.
    (
        "admin/handlers/collections/item/versions/list.rs",
        "ops::find_document_by_id(",
    ),
];

fn is_allowlisted(rel_path: &str, call: &str) -> bool {
    ALLOWLIST
        .iter()
        .any(|(suffix, allowed_call)| rel_path.ends_with(suffix) && *allowed_call == call)
}

/// Collect `.rs` files under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
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

#[test]
fn surfaces_do_not_bypass_the_service_layer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations: Vec<String> = Vec::new();

    for surface in SURFACE_ROOTS {
        let mut files = Vec::new();
        rust_files(&root.join(surface), &mut files);

        for file in files {
            let contents = fs::read_to_string(&file).unwrap_or_default();
            let rel = file
                .strip_prefix(root)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");

            for (lineno, line) in contents.lines().enumerate() {
                // Skip line comments — references in prose/docs aren't calls.
                if line.trim_start().starts_with("//") {
                    continue;
                }

                for call in FORBIDDEN_CALLS {
                    if line.contains(call) && !is_allowlisted(&rel, call) {
                        violations.push(format!("  {}:{}  {}", rel, lineno + 1, call));
                    }
                }
            }
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
    // write would defeat validation / hooks / ref-counting.
    let write_calls = [
        "query::create(",
        "query::update",
        "query::delete(",
        "query::soft_delete(",
        "query::undelete(",
        "query::create_version(",
    ];
    for (path, call) in ALLOWLIST {
        for w in write_calls {
            assert!(
                !call.contains(w),
                "ALLOWLIST entry for {path} permits a write primitive ({call}); \
                 writes must never bypass the service layer"
            );
        }
    }
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
// the service layer; a handful of non-CRUD surface ops (streams, file serving)
// have no service op and call the evaluator directly. This guard freezes that
// small set of surface-level `check_access` touchpoints: a new one fails CI,
// forcing review of whether it should instead go through a service op — and
// preventing a surface from growing its own ad-hoc access logic.

/// Surface files allowed to call `check_access` directly, each a reviewed
/// non-CRUD touchpoint that delegates to the shared evaluator.
const ACCESS_TOUCHPOINTS: &[&str] = &[
    // gRPC adapter's access-check wrapper — delegates to `hook_runner.check_access`.
    "api/handlers/content_service.rs",
    // Subscribe stream: no service CRUD op exists, so it checks read access directly.
    "api/handlers/subscribe.rs",
    // SSE event stream: same — no service CRUD op.
    "admin/handlers/events/sse.rs",
    // Authenticated upload file serving: same — no service CRUD op.
    "admin/handlers/uploads/serve.rs",
    // Admin's shared access helpers (the admin-side centralization point).
    "admin/handlers/shared/access.rs",
];

#[test]
fn surface_access_checks_are_frozen_to_reviewed_touchpoints() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations: Vec<String> = Vec::new();

    for surface in SURFACE_ROOTS {
        let mut files = Vec::new();
        rust_files(&root.join(surface), &mut files);

        for file in files {
            let contents = fs::read_to_string(&file).unwrap_or_default();
            let rel = file
                .strip_prefix(root)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");

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
