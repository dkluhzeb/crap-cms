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
//! allowlist with its reason.
//!
//! Each allowlist row carries the NUMBER of dispatches its file is reviewed
//! for, and the count must match exactly: too few means the row went stale,
//! too many means an allowlisted file grew a second, unreviewed dispatch —
//! which is how a hand-rolled descent would otherwise arrive for free.
//!
//! The scan sees three spellings of a dispatch: a scrutinee ending in
//! `.field_type`, one bound to a local first (`let ft = &f.field_type;` then
//! `match ft {`), and a `field_type` value passed in by name. The arm block may
//! open on the following line. Textual-scan limits apply, as documented in
//! `surface_parity.rs`: comments and test modules are removed first (see
//! `common::production_code`), but a `match` keyword inside a string literal
//! still starts a scrutinee scan — it would have to be followed by
//! `.field_type` before the next brace to count.

mod common;

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use crate::common::production_code;

/// Every production file allowed to contain a `match …field_type { … }`: the
/// number of dispatches reviewed there, and why they are not hand-rolled
/// composite descent.
const ALLOWLIST: &[(&str, usize, &str)] = &[
    (
        "src/core/field/storage.rs",
        1,
        "per-field value mapping (which column forms a read decodes), no tree descent",
    ),
    // The classifier itself, and the canonical in-row walker.
    (
        "src/core/walk.rs",
        1,
        "field_children — THE FieldType → sub-tree classifier",
    ),
    // Leaf value-dispatch INSIDE a `field_children` match (Relationship / Upload
    // / Join each carry a distinct leaf action — not tree descent).
    (
        "src/db/query/populate/single/nested.rs",
        1,
        "Leaf re-dispatch under a field_children match (rel/upload/join populate)",
    ),
    (
        "src/db/query/read/back_references/scan.rs",
        1,
        "Leaf re-dispatch under a field_children match (rel/upload back-ref scan)",
    ),
    // Per-field value mappings — one field → one value/column/schema, no descent.
    (
        "src/admin/handlers/collections/list_helpers.rs",
        1,
        "per-field list-column render (value map)",
    ),
    (
        "src/admin/handlers/field_context/enrich/field_types.rs",
        2,
        "per-variant FieldContext construction / enrichment (value map)",
    ),
    (
        "src/admin/handlers/field_context/enrich/nested.rs",
        2,
        "enrich walk zips FieldContext with its defs — needs both, cannot use the classifier",
    ),
    (
        "src/admin/handlers/forms/join_data.rs",
        1,
        "per-leaf join-data extraction under walk_leaf_fields (value map)",
    ),
    (
        "src/commands/bench/helpers.rs",
        1,
        "per-type synthetic bench value (value map)",
    ),
    (
        "src/core/field/definition.rs",
        1,
        "has_parent_column() predicate (value map)",
    ),
    (
        "src/core/text.rs",
        1,
        "canonical_text — per-type canonical storage form of a scalar value (value map)",
    ),
    (
        "src/db/migrate/checkbox_columns.rs",
        1,
        "per-leaf checkbox-column collection under walk_leaf_fields (value map)",
    ),
    (
        "src/db/migrate/helpers/join_tables/orchestrator.rs",
        1,
        "per-leaf JoinTableKind under walk_leaf_fields (value map)",
    ),
    (
        "src/db/query/filter/resolve/path.rs",
        2,
        "root-field resolver dispatch — delegates, does not descend",
    ),
    (
        "src/db/query/helpers/coerce.rs",
        1,
        "coerce_value — per-type form-string → DbValue coercion (value map)",
    ),
    (
        "src/hooks/lifecycle/validation/checks/has_many.rs",
        1,
        "per-value has-many element validation (value map)",
    ),
    (
        "src/hooks/lifecycle/validation/checks/required.rs",
        1,
        "per-value required-field check (value map)",
    ),
    (
        "src/hooks/lua_api/parse/fields/constraints.rs",
        1,
        "per-type expected default_value JSON type (value map)",
    ),
    (
        "src/hooks/lua_api/parse/fields/single.rs",
        1,
        "type_specific_field_keys — per-type allowed schema keys, exhaustive (value map)",
    ),
    (
        "src/mcp/schema.rs",
        1,
        "field_schema — per-type JSON Schema (value map)",
    ),
    (
        "src/admin/handlers/field_context/builder/single.rs",
        1,
        "construct_field_variant — per-variant typed FieldContext constructor (value map)",
    ),
    (
        "src/scaffold/collection/writer.rs",
        1,
        "type_specific_stub — per-type scaffold stub, keyed on the type NAME string (value map)",
    ),
    (
        "src/scaffold/hook/generator.rs",
        1,
        "condition_table_body — per-type sample condition, keyed on the type NAME string (value map)",
    ),
    (
        "src/typegen/client/driver.rs",
        1,
        "resolve_ty — per-type client FieldTy render (value map)",
    ),
    (
        "src/typegen/lua/field.rs",
        1,
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

/// True for a character that can appear in an identifier.
fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// True when `chars[i..]` starts with `word` and neither neighbour is an
/// identifier character.
fn word_at(chars: &[char], i: usize, word: &str) -> bool {
    if i > 0 && is_ident(chars[i - 1]) {
        return false;
    }

    let spelled = word
        .chars()
        .enumerate()
        .all(|(n, c)| chars.get(i + n) == Some(&c));

    spelled && chars.get(i + word.len()).is_none_or(|c| !is_ident(*c))
}

/// The scrutinee text of every `match … {` in scrubbed `code`.
///
/// The scrutinee runs to the `{` that opens the arm block, so it survives a
/// line break between the two. Braces inside literals are already neutralized,
/// so only a structural brace ends it.
fn match_scrutinees(code: &str) -> Vec<String> {
    let chars: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        if !word_at(&chars, i, "match") {
            i += 1;
            continue;
        }

        let start = i + "match".len();
        let mut depth = 0i32;
        let mut j = start;

        while j < chars.len() {
            match chars[j] {
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                ';' if depth == 0 => break,
                '{' if depth == 0 => {
                    out.push(chars[start..j].iter().collect());
                    break;
                }
                _ => {}
            }

            j += 1;
        }

        i = start;
    }

    out
}

/// Identifiers bound directly to a field's type: `let ft = &f.field_type;`
/// or `let ft = f.field_type.clone();`.
///
/// Only a binding whose whole right-hand side is the `field_type` access
/// counts — `let schema = match f.field_type { … };` binds a schema, not a
/// field type.
fn field_type_idents(code: &str) -> HashSet<String> {
    let chars: Vec<char> = code.chars().collect();
    let mut out = HashSet::new();
    let mut i = 0;

    while i < chars.len() {
        if !word_at(&chars, i, "let") {
            i += 1;
            continue;
        }

        let start = i + "let".len();
        let end = chars[start..]
            .iter()
            .position(|c| *c == ';')
            .map_or(chars.len(), |n| start + n);
        let statement: String = chars[start..end].iter().collect();

        let statement = statement.trim_end();
        let statement = statement.strip_suffix(".clone()").unwrap_or(statement);

        if statement.ends_with(".field_type") {
            out.extend(leading_ident(statement));
        }

        i = start;
    }

    out
}

/// The identifier a `let` statement binds, `mut` skipped. `None` for a
/// destructuring pattern that does not start with a plain name.
fn leading_ident(statement: &str) -> Option<String> {
    let trimmed = statement.trim_start();
    let name = trimmed.strip_prefix("mut ").unwrap_or(trimmed).trim_start();
    let ident: String = name.chars().take_while(|c| is_ident(*c)).collect();

    (!ident.is_empty()).then_some(ident)
}

/// How many `match … field_type { … }` dispatches `code` contains.
fn count_dispatches(code: &str) -> usize {
    let bound = field_type_idents(code);

    match_scrutinees(code)
        .iter()
        .filter(|scrutinee| is_field_type_dispatch(scrutinee.as_str(), &bound))
        .count()
}

/// True when `scrutinee` selects on a field's type.
fn is_field_type_dispatch(scrutinee: &str, bound: &HashSet<String>) -> bool {
    if scrutinee.contains(".field_type") {
        return true;
    }

    let ident = scrutinee.trim().trim_start_matches(['&', '*']).trim();

    ident == "field_type" || bound.contains(ident)
}

/// Every production file with at least one dispatch, and how many it has.
fn dispatch_counts(root: &Path) -> Vec<(String, usize)> {
    let mut files = Vec::new();
    rs_files(&root.join("src"), &mut files);
    files.sort();

    let mut counts = Vec::new();

    for path in &files {
        let Ok(src) = fs::read_to_string(path) else {
            continue;
        };

        let count = count_dispatches(&production_code(&src));

        if count > 0 {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");

            counts.push((rel, count));
        }
    }

    counts
}

#[test]
fn every_field_type_dispatch_is_reviewed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders: Vec<String> = Vec::new();

    for (rel, count) in dispatch_counts(root) {
        let found = ALLOWLIST.iter().find(|(file, _, _)| *file == rel.as_str());

        let Some((_, reviewed, _)) = found else {
            offenders.push(format!("{rel} ({count} dispatch site(s), unreviewed)"));
            continue;
        };

        if *reviewed != count {
            offenders.push(format!(
                "{rel} (reviewed for {reviewed} dispatch site(s), found {count})"
            ));
        }
    }

    offenders.sort();

    assert!(
        offenders.is_empty(),
        "unreviewed `match <field>.field_type {{ … }}` dispatch in production code:\n  {}\n\n\
         If it descends the field tree, route the structural dispatch through \
         `core::walk::field_children` (so a new FieldType is a compile error at the one \
         classifier). If it is a per-field value mapping with no tree descent, add it to \
         ALLOWLIST in tests/field_tree_dispatch.rs — or correct that file's reviewed count — \
         with the reason.",
        offenders.join("\n  ")
    );
}

#[test]
fn allowlist_entries_still_exist_and_match() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let found = dispatch_counts(root);

    for (rel, reviewed, reason) in ALLOWLIST {
        let path = root.join(rel);

        assert!(
            path.exists(),
            "Allowlisted field_type-dispatch file no longer exists: {rel} ({reason}). \
             Remove the stale ALLOWLIST row in tests/field_tree_dispatch.rs."
        );

        let count = found
            .iter()
            .find(|(file, _)| file.as_str() == *rel)
            .map_or(0, |(_, count)| *count);

        assert_eq!(
            count, *reviewed,
            "Allowlisted file {rel} holds {count} production `match …field_type {{` site(s), \
             not the {reviewed} it is reviewed for ({reason}). Update the count — or remove the \
             row if the dispatch is gone — so the inventory does not rot into a vacuous pin."
        );
    }
}

/// Positive control: the matcher sees every spelling of a field-type dispatch
/// the inventory claims to cover, and nothing else.
#[test]
fn matcher_sees_every_dispatch_spelling() {
    let cases: &[(&str, usize, &str)] = &[
        (
            "fn a() { match f.field_type { _ => () } }",
            1,
            "plain field access",
        ),
        (
            "fn a() { match field\n    .field_type\n{ _ => () } }",
            1,
            "arm block opening on the following line",
        ),
        (
            "fn a() { let ft = &f.field_type;\n match ft { _ => () } }",
            1,
            "field type bound to a local first",
        ),
        (
            "fn a(field_type: &FieldType) { match field_type { _ => () } }",
            1,
            "field type passed in by name",
        ),
        (
            "fn a() { match cf.field_type.as_str() { _ => () } }",
            1,
            "field type reached through an accessor",
        ),
        (
            "fn a() { // match f.field_type { _ => () }\n }",
            0,
            "a dispatch named only in a comment",
        ),
        (
            "fn a() { let schema = match f.field_type { _ => () };\n match schema { _ => () } }",
            1,
            "a local bound to the RESULT of a dispatch is not itself one",
        ),
        ("fn a() { match f.kind { _ => () } }", 0, "another field"),
        (
            "fn a() {}\n#[cfg(all(test, feature = \"sqlite\"))]\nmod tests {\n \
             fn t() { match f.field_type { _ => () } }\n}",
            0,
            "a dispatch inside a feature-gated test module",
        ),
        (
            "//! A doc comment naming #[cfg(test)] must not truncate the file.\n\
             fn a() { match f.field_type { _ => () } }",
            1,
            "production code below a doc comment that names the test gate",
        ),
    ];

    for (source, expected, what) in cases {
        assert_eq!(
            count_dispatches(&production_code(source)),
            *expected,
            "matcher must see {expected} dispatch(es) for {what}: {source}"
        );
    }
}
