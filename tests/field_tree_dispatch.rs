//! Field-tree composite-dispatch inventory.
//!
//! The mapping from `FieldType` to its structural sub-tree (Group nests,
//! Row/Collapsible/Tabs are transparent wrappers, Array/Blocks are
//! repeatable rows) must live in exactly ONE place: `core::walk::field_children`
//! (plus the canonical in-row `FieldHookWalker`). Every walker that descends the
//! field tree routes its structural dispatch through that classifier, so a new
//! `FieldType` is a compile error at the one classifier instead of being
//! silently leaf-classified (a missed ref-count, a skipped validation, an
//! unrendered nested field — all of which really happened).
//!
//! A hand-rolled `match <field>.field_type { FieldType::Group => …recurse… }`
//! re-implements that classification and carries a `_ =>` wildcard that swallows
//! whatever container variant the author forgot. This test is the reviewed
//! inventory of every production `match …field_type { … }` site: each is either
//! the sanctioned classifier, a leaf value-dispatch *inside* a `field_children`
//! match, or a genuine per-field value mapping (no tree descent). A NEW file
//! matching on `field_type` fails here and forces the review: route composite
//! descent through `field_children`, or add the value-mapping site to the
//! allowlist with its reason. Textual-scan limits apply, as documented in
//! `surface_parity.rs`.

use std::fs;
use std::path::{Path, PathBuf};

/// Every production file allowed to contain a `match …field_type { … }`, with
/// the reason it is not hand-rolled composite descent.
const ALLOWLIST: &[(&str, &str)] = &[
    // The classifier itself, and the canonical in-row walker.
    (
        "src/core/walk.rs",
        "field_children — THE FieldType → sub-tree classifier",
    ),
    (
        "src/hooks/lifecycle/execution/field_hooks.rs",
        "FieldHookWalker — the canonical in-row hook walker",
    ),
    // Leaf value-dispatch INSIDE a `field_children` match (Relationship / Upload
    // / Join each carry a distinct leaf action — not tree descent).
    (
        "src/db/query/populate/single/nested.rs",
        "Leaf re-dispatch under a field_children match (rel/upload/join populate)",
    ),
    (
        "src/db/query/read/back_references/scan.rs",
        "Leaf re-dispatch under a field_children match (rel/upload back-ref scan)",
    ),
    // Per-field value mappings — one field → one value/column/schema, no descent.
    (
        "src/admin/handlers/collections/list_helpers.rs",
        "per-field list-column render (value map)",
    ),
    (
        "src/admin/handlers/field_context/enrich/field_types.rs",
        "per-variant FieldContext construction / enrichment (value map)",
    ),
    (
        "src/admin/handlers/field_context/enrich/nested.rs",
        "enrich walk zips FieldContext with its defs — needs both, cannot use the classifier",
    ),
    (
        "src/admin/handlers/forms/join_data.rs",
        "per-leaf join-data extraction under walk_leaf_fields (value map)",
    ),
    (
        "src/commands/bench/helpers.rs",
        "per-type synthetic bench value (value map)",
    ),
    (
        "src/core/field/definition.rs",
        "has_parent_column() predicate (value map)",
    ),
    (
        "src/db/migrate/checkbox_columns.rs",
        "per-leaf checkbox-column collection under walk_leaf_fields (value map)",
    ),
    (
        "src/db/migrate/helpers/join_tables/orchestrator.rs",
        "per-leaf JoinTableKind under walk_leaf_fields (value map)",
    ),
    (
        "src/db/query/filter/resolve/path.rs",
        "root-field resolver dispatch — delegates, does not descend",
    ),
    (
        "src/hooks/lifecycle/validation/checks/has_many.rs",
        "per-value has-many element validation (value map)",
    ),
    (
        "src/hooks/lifecycle/validation/checks/required.rs",
        "per-value required-field check (value map)",
    ),
    (
        "src/mcp/schema.rs",
        "field_to_json_schema — per-type JSON Schema (value map)",
    ),
    (
        "src/admin/handlers/field_context/builder/single.rs",
        "construct_field_variant — per-variant typed FieldContext constructor (value map)",
    ),
    (
        "src/typegen/client/driver.rs",
        "resolve_ty — per-type client FieldTy render (value map)",
    ),
    (
        "src/typegen/lua/field.rs",
        "field_to_lua_type — per-type Lua type-string render (value map)",
    ),
];

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

/// True if `line` (already trimmed) opens a composite dispatch on a field's
/// `field_type` — `match <expr>.field_type {` — and is not a comment.
fn is_field_type_match(line: &str) -> bool {
    let trimmed = line.trim_start();

    if trimmed.starts_with("//") || trimmed.starts_with('*') {
        return false;
    }

    // `match … .field_type {` — the scrutinee ends in `.field_type` and the arm
    // block opens on the same line.
    let Some(rest) = trimmed
        .strip_prefix("match ")
        .or_else(|| trimmed.split(" match ").nth(1))
    else {
        return trimmed.contains("match ") && trimmed.contains(".field_type {");
    };

    rest.contains(".field_type {") || rest.trim_end().ends_with(".field_type")
}

#[test]
fn every_field_type_dispatch_is_reviewed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&root, &mut files);

    let allowed: std::collections::HashSet<&str> = ALLOWLIST.iter().map(|(f, _)| *f).collect();

    let mut offenders: Vec<String> = Vec::new();

    for path in &files {
        let Ok(src) = fs::read_to_string(path) else {
            continue;
        };

        // Production code only: stop at the first `#[cfg(test)]`, which by
        // convention gates the file's test module at the bottom.
        let production = src.split("#[cfg(test)]").next().unwrap_or(&src);

        if !production.lines().any(is_field_type_match) {
            continue;
        }

        let rel = path
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        if !allowed.contains(rel.as_str()) {
            offenders.push(rel);
        }
    }

    offenders.sort();

    assert!(
        offenders.is_empty(),
        "New `match <field>.field_type {{ … }}` dispatch found in production code:\n  {}\n\n\
         If it descends the field tree, route the structural dispatch through \
         `core::walk::field_children` (so a new FieldType is a compile error at the one \
         classifier). If it is a per-field value mapping with no tree descent, add it to \
         ALLOWLIST in tests/field_tree_dispatch.rs with the reason.",
        offenders.join("\n  ")
    );
}

#[test]
fn allowlist_entries_still_exist_and_match() {
    let root = env!("CARGO_MANIFEST_DIR");

    for (rel, reason) in ALLOWLIST {
        let path = Path::new(root).join(rel);

        assert!(
            path.exists(),
            "Allowlisted field_type-dispatch file no longer exists: {rel} ({reason}). \
             Remove the stale ALLOWLIST row in tests/field_tree_dispatch.rs."
        );

        let src = fs::read_to_string(&path).expect("read allowlisted file");
        let production = src.split("#[cfg(test)]").next().unwrap_or(&src);

        assert!(
            production.lines().any(is_field_type_match),
            "Allowlisted file {rel} no longer contains a production `match …field_type {{`. \
             The dispatch it documented ({reason}) is gone — remove the stale ALLOWLIST row \
             so the inventory does not rot into a vacuous pin."
        );
    }
}
