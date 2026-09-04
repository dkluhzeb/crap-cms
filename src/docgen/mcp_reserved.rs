//! Generate the MCP reserved-arguments table (`docs/src/mcp/overview.md`,
//! region `mcp-reserved-args`) from the single-source wire model — the
//! tools column drifted every time an op gained an option, because the
//! mdbook table was a hand-written fourth copy of the wire surface.

use std::fmt::Write as _;

use crate::service::op::wire::{COLLECTION_OPS, GLOBAL_OPS, WireSurfaces};

/// The reserved top-level write arguments the table documents, with their
/// prose. The *prose* is static (it summarizes cross-op behavior); the
/// *tools* column is derived from the wire model per render, so coverage
/// can never drift again.
const RESERVED: &[(&str, &str)] = &[
    (
        "locale",
        "Locale code for localized fields — selects the locale on reads, targets it on writes.",
    ),
    (
        "draft",
        "On writes: save as a draft version. On reads: include the draft overlay.",
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
];

/// MCP tool-name pattern for a wire op.
fn tool_pattern(op: &str) -> String {
    match op {
        "get_global" => "`global_read_*`".to_string(),
        "update_global" => "`global_update_*`".to_string(),
        "validate_global" => "`global_validate_*`".to_string(),
        other => format!("`{other}_*`"),
    }
}

/// Render the reserved-arguments Markdown table.
#[must_use]
pub fn generate_mcp_reserved_args_table() -> String {
    let mut out =
        String::from("| Argument | Tools | Description |\n|----------|-------|-------------|\n");

    for (arg, doc) in RESERVED {
        let mut tools = Vec::new();

        for op in COLLECTION_OPS.iter().chain(GLOBAL_OPS) {
            let exposed = op
                .fields
                .iter()
                .any(|f| f.name == *arg && f.surfaces.contains(WireSurfaces::MCP));
            if exposed {
                tools.push(tool_pattern(op.op));
            }
        }

        let _ = writeln!(out, "| `{arg}` | {} | {doc} |", tools.join(", "));
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
}
