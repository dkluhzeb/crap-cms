//! Render the operation-options reference (`docs/src/reference/operation-options.md`)
//! from the wire model — Phase D of the single-source schema program.
//!
//! One table per operation: field, wire type, requiredness, surfaces, and
//! description, straight from [`crate::service::op::wire`]. Regenerated via
//! `cargo xtask gen-wire-doc`; CI gates on `--check`, and an in-crate test
//! asserts the committed file is in sync.

use std::fmt::Write as _;

use crate::service::op::wire::{COLLECTION_OPS, GLOBAL_OPS, OpWire, WireKind, WireSurfaces};

/// Human name for a wire kind, as shown in the reference table.
fn kind_label(kind: WireKind) -> &'static str {
    match kind {
        WireKind::Bool => "boolean",
        WireKind::Int => "integer",
        WireKind::Str => "string",
        WireKind::Id => "id (string)",
        WireKind::Locale => "locale (string)",
        WireKind::Select => "string[]",
        WireKind::FilterMap => "where filter",
        WireKind::DataFields => "field data (top-level)",
        WireKind::DataObject => "field data (`data` object)",
        WireKind::DocumentsArray => "field data (`documents` array)",
    }
}

/// Surface list for one field, e.g. `gRPC, MCP, Lua`.
fn surfaces_label(s: WireSurfaces) -> String {
    let mut parts = Vec::new();
    if s.contains(WireSurfaces::GRPC) {
        parts.push("gRPC");
    }
    if s.contains(WireSurfaces::MCP) {
        parts.push("MCP");
    }
    if s.contains(WireSurfaces::LUA) {
        parts.push("Lua");
    }
    parts.join(", ")
}

fn render_op(out: &mut String, w: &OpWire) {
    let _ = writeln!(out, "### `{}`\n", w.op);
    out.push_str("| Field | Type | Required | Surfaces | Description |\n");
    out.push_str("|-------|------|----------|----------|-------------|\n");

    for field in w.fields {
        let name = if field.grpc_name() == field.name {
            format!("`{}`", field.name)
        } else {
            format!("`{}` (gRPC: `{}`)", field.name, field.grpc_name())
        };
        let required = if field.required { "yes" } else { "" };
        let doc = field.doc.replace('|', "\\|");
        let _ = writeln!(
            out,
            "| {name} | {} | {required} | {} | {doc} |",
            kind_label(field.kind),
            surfaces_label(field.surfaces),
        );
    }

    out.push('\n');
}

/// Generate the full Markdown reference page.
#[must_use]
pub fn generate_wire_reference_md() -> String {
    let mut out = String::with_capacity(16 * 1024);

    out.push_str(
        "<!-- GENERATED FILE — do not edit. Regenerate with `cargo xtask gen-wire-doc`. -->\n\n\
         # Operation options reference\n\n\
         The wire options of every CRUD operation, generated from the\n\
         single-source wire model (`service::op::wire`) — the same model that\n\
         renders the MCP tool schemas and that the wire-parity test checks\n\
         `proto/content.proto` and `types/crap.lua` against.\n\n\
         Conventions:\n\n\
         - **Surfaces** — where the option exists. Routing (the collection\n\
           slug in the gRPC message / MCP tool name / Lua argument), Lua's\n\
           `override_access`, and Lua's positional arguments (`id`, `data`,\n\
           `documents`, `version_id`) are structural per surface and not\n\
           listed as options.\n\
         - **where filter** — the canonical filter grammar: an object on\n\
           MCP/Lua, a JSON string on gRPC.\n\
         - **field data** — the document payload, shaped by the collection's\n\
           field definitions.\n\
         - `unpublish` has no gRPC RPC of its own — gRPC spells it as the\n\
           `unpublish` flag on `update`.\n\n\
         ## Collection operations\n\n",
    );

    for w in COLLECTION_OPS {
        render_op(&mut out, w);
    }

    out.push_str("## Global operations\n\n");

    for w in GLOBAL_OPS {
        render_op(&mut out, w);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed reference page must match what the model renders —
    /// mirrors the xtask `--check` gate so plain `cargo test` catches drift.
    #[test]
    fn wire_reference_doc_is_in_sync() {
        let generated = generate_wire_reference_md();
        let committed = include_str!("../../../docs/src/reference/operation-options.md");
        assert_eq!(
            committed, generated,
            "docs/src/reference/operation-options.md is stale — run `cargo xtask gen-wire-doc`"
        );
    }
}
