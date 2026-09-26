//! Generate the MCP reserved-arguments table (`docs/src/mcp/overview.md`,
//! region `mcp-reserved-args`) from the single-source wire model — the table
//! was a hand-written fourth copy of the wire surface and drifted every time
//! an op gained an option (`trash` was missing from it entirely).

use std::fmt::Write as _;

use crate::service::op::wire::{
    COLLECTION_OPS, GLOBAL_OPS, OpWire, WireField, WireKind, WireSurfaces,
};

/// Prose for each reserved argument, keyed by its wire-model name.
///
/// The ROWS are derived from the wire model (see [`reserved_arguments`]); only
/// the prose lives here, because it summarizes cross-op behavior no single
/// `WireField::doc` states. The test below pins the map to the model in both
/// directions: every reserved argument has prose, and no prose names an
/// argument the model no longer has.
const DESCRIPTIONS: &[(&str, &str)] = &[
    (
        "id",
        "Target document ID — addressed as its own argument, never as field data.",
    ),
    (
        "where",
        "Filter conditions as a JSON object (`{\"status\": {\"equals\": \"draft\"}}`), not the JSON-encoded string the gRPC field uses.",
    ),
    ("order_by", "Sort field; prefix with `-` for descending."),
    ("limit", "Maximum results per page."),
    ("page", "Page number, 1-indexed (page mode only)."),
    (
        "after_cursor",
        "Forward cursor (cursor mode only; mutually exclusive with `page` and `before_cursor`).",
    ),
    (
        "before_cursor",
        "Backward cursor (cursor mode only; mutually exclusive with `page` and `after_cursor`).",
    ),
    ("depth", "How deep to populate relationships."),
    ("search", "Full-text search query."),
    (
        "select",
        "Field names to return (projection); omit for all fields.",
    ),
    (
        "locale",
        "Locale code for localized fields — selects the locale on reads, targets it on writes.",
    ),
    (
        "draft",
        "On writes: save as a draft version. On reads: include the draft overlay.",
    ),
    (
        "trash",
        "Read the soft-deleted documents instead of the live ones (the trash view).",
    ),
    (
        "events",
        "Publish live events for this write. Defaults to `true` on single-document tools and `false` on the bulk (`*_many_*`) tools.",
    ),
    (
        "hooks",
        "Run lifecycle hooks per item (default `true`). Bulk-only; single-document tools always run hooks.",
    ),
    (
        "force_hard_delete",
        "Skip `soft_delete` and remove the row permanently.",
    ),
    (
        "queue",
        "Run as a queued background job: returns a `job_id` instead of results; poll it with the `get_job_run` tool. Advertised and accepted only when `[mcp] job_tools` is `\"read\"` or `\"all\"`.",
    ),
    (
        "documents",
        "The array of documents to create — each item carries that document's own field data.",
    ),
    (
        "data",
        "The field values to apply to every matching document.",
    ),
    ("version_id", "The version snapshot to restore from."),
    (
        "expected_revision",
        "The document's `_revision` as last read: when the document was written since, the write is refused with a `Revision conflict` error and nothing changes. Omit to write unconditionally.",
    ),
];

/// Every op this table covers, collections first, then globals.
fn ops() -> impl Iterator<Item = &'static OpWire> {
    COLLECTION_OPS.iter().chain(GLOBAL_OPS)
}

/// Is this wire field a reserved TOP-LEVEL argument on the MCP surface?
///
/// [`WireKind::DataFields`] is the one exception: that op's document data is
/// spread across the input object's top level rather than nested under a
/// property, so the model's `data` name is not a key on the wire there — the
/// MCP schema emitter skips it for exactly the same reason.
fn is_reserved_mcp_argument(field: &WireField) -> bool {
    field.surfaces.contains(WireSurfaces::MCP) && field.kind != WireKind::DataFields
}

/// The reserved arguments in first-appearance order across the wire model.
fn reserved_arguments() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = Vec::new();

    for field in ops().flat_map(|op| op.fields) {
        if is_reserved_mcp_argument(field) && !names.contains(&field.name) {
            names.push(field.name);
        }
    }

    names
}

/// The description of `arg`: the MCP-specific prose in [`DESCRIPTIONS`] when
/// there is one, otherwise the wire field's own doc — the same text the MCP
/// schemas surface — so an argument can never appear without one.
fn description(arg: &str) -> &'static str {
    if let Some((_, doc)) = DESCRIPTIONS.iter().find(|(name, _)| *name == arg) {
        return doc;
    }

    ops()
        .flat_map(|op| op.fields)
        .find(|f| f.name == arg && is_reserved_mcp_argument(f))
        .map_or("", |f| f.doc)
}

/// MCP tool-name pattern for a wire op.
fn tool_pattern(op: &str) -> String {
    match op {
        "get_global" => "`global_read_*`".to_string(),
        "update_global" => "`global_update_*`".to_string(),
        "validate_global" => "`global_validate_*`".to_string(),
        other => format!("`{other}_*`"),
    }
}

/// The tools that spell `arg`, as their MCP tool-name patterns.
fn tools_for(arg: &str) -> Vec<String> {
    ops()
        .filter(|op| {
            op.fields
                .iter()
                .any(|f| f.name == arg && is_reserved_mcp_argument(f))
        })
        .map(|op| tool_pattern(op.op))
        .collect()
}

/// Render the reserved-arguments Markdown table.
#[must_use]
pub fn generate_mcp_reserved_args_table() -> String {
    let mut out =
        String::from("| Argument | Tools | Description |\n|----------|-------|-------------|\n");

    for arg in reserved_arguments() {
        let doc = description(arg);
        let _ = writeln!(out, "| `{arg}` | {} | {doc} |", tools_for(arg).join(", "));
    }

    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_column_derives_from_the_wire_model() {
        let md = generate_mcp_reserved_args_table();
        assert!(md.contains("| `locale` |"), "{md}");
        assert!(md.contains("`create_*`"), "{md}");
        assert!(md.contains("`global_update_*`"), "{md}");
        // hooks is bulk-only.
        let hooks_row = md.lines().find(|l| l.starts_with("| `hooks`")).unwrap();
        assert!(hooks_row.contains("many"), "{hooks_row}");
        assert!(!hooks_row.contains("`create_*`"), "{hooks_row}");
    }

    /// Regression: the rows were a hand-written list and `trash` — a reserved
    /// argument on three read tools — was never in it. Rows now come from the
    /// wire model, and only the prose is hand-written: every reserved argument
    /// must have prose, and no prose may name a non-argument.
    #[test]
    fn every_reserved_argument_has_a_description() {
        let args = reserved_arguments();
        assert!(args.contains(&"trash"), "derived rows: {args:?}");

        for arg in &args {
            assert!(
                !description(arg).is_empty(),
                "reserved argument '{arg}' has no description"
            );
        }

        for (name, _) in DESCRIPTIONS {
            assert!(
                args.contains(name),
                "DESCRIPTIONS names '{name}', which is not a reserved argument"
            );
        }
    }

    /// The `create`/`update`/`validate` document data is spread at the top
    /// level, so `data` is a reserved KEY only on the ops that nest it
    /// (`update_many`) — the same split the MCP schema emitter makes.
    #[test]
    fn spread_document_data_is_not_a_reserved_key() {
        let md = generate_mcp_reserved_args_table();
        let data_row = md
            .lines()
            .find(|l| l.starts_with("| `data`"))
            .expect("update_many nests its data under a `data` property");

        assert!(data_row.contains("`update_many_*`"), "{data_row}");
        assert!(
            !data_row.contains("`create_*`"),
            "create spreads its data at the top level: {data_row}"
        );
    }
}
