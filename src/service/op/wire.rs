//! The single-source wire model: each operation's option fields declared
//! ONCE, consumed by every surface description.
//!
//! Before this model, each operation's wire surface was hand-described three
//! times — the proto message, the MCP JSON schema, and the Lua option
//! structs — plus a fourth copy in the mdbook tables. Every wire-parity bug
//! of the alpha.10 cycle (`select`/`search`/`locale` missing on MCP,
//! `events` missing on undelete, `draft` missing on `UpdateGlobal`, …) was
//! one description updated and its siblings forgotten.
//!
//! The model describes the *wire options* — the drift-prone part. Document
//! DATA payloads stay def-dependent (the MCP emitter embeds the collection's
//! field schema via [`WireKind::DataFields`]-class kinds), and routing
//! (collection slug in the gRPC message / MCP tool name / Lua first arg) is
//! structural per surface, not a field here.
//!
//! Consumers:
//! - the MCP input schemas are built from this model at runtime
//!   (`src/mcp/schema.rs`) — no hand-written JSON per op;
//! - the wire-parity checker diffs the remaining hand sources
//!   (`proto/content.proto`, `types/crap.lua`) against it;
//! - (planned) the proto generator and the mdbook tables render from it.

/// Which surfaces expose a field. Routing/structural differences aside, most
/// fields exist everywhere; the exceptions are explicit.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct WireSurfaces(u8);

impl WireSurfaces {
    pub const GRPC: WireSurfaces = WireSurfaces(0b001);
    pub const MCP: WireSurfaces = WireSurfaces(0b010);
    pub const LUA: WireSurfaces = WireSurfaces(0b100);
    /// The common case.
    pub const ALL: WireSurfaces = WireSurfaces(0b111);
    /// Lua-only (e.g. `override_access`, `hooks` on single reads — trusted
    /// in-process surface options).
    pub const LUA_ONLY: WireSurfaces = WireSurfaces(0b100);
    /// Everything except Lua.
    pub const GRPC_MCP: WireSurfaces = WireSurfaces(0b011);
    /// Everything except gRPC (wire-schema gaps that are decisions, not bugs).
    pub const MCP_LUA: WireSurfaces = WireSurfaces(0b110);
    /// Everything except MCP.
    pub const GRPC_LUA: WireSurfaces = WireSurfaces(0b101);

    #[must_use]
    pub fn contains(self, s: WireSurfaces) -> bool {
        self.0 & s.0 == s.0
    }
}

/// The wire type of an option field. Each emitter maps a kind onto its
/// surface's spelling (JSON schema type, proto scalar, Lua annotation).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WireKind {
    /// Boolean flag.
    Bool,
    /// Integer (i64 on the wire).
    Int,
    /// Plain string.
    Str,
    /// A document id.
    Id,
    /// Locale code (`"en"`, `"de"`, `"all"` where reads allow it).
    Locale,
    /// Field-name projection list.
    Select,
    /// The canonical `where` filter grammar (object on MCP/Lua, JSON string
    /// on gRPC).
    FilterMap,
    /// Document field data spread at the TOP level of the input object
    /// (create/update/validate) — def-dependent, embedded by the emitter.
    DataFields,
    /// Document field data wrapped in a `data` property (`update_many`).
    DataObject,
    /// Array of per-item document objects (`create_many` `documents`).
    DocumentsArray,
}

/// One wire option field of one operation.
#[derive(Clone, Copy)]
pub struct WireField {
    pub name: &'static str,
    pub kind: WireKind,
    pub required: bool,
    /// Human description — the exact text surfaced in MCP schemas and docs
    /// (defaults are stated inline, matching the established wording).
    pub doc: &'static str,
    pub surfaces: WireSurfaces,
    /// Proto spelling when it differs from `name` (e.g. `restore_version`'s
    /// `id` is `document_id` on the wire). `None` = same as `name`.
    pub grpc_name: Option<&'static str>,
}

impl WireField {
    /// The field's name on the gRPC wire.
    #[must_use]
    pub fn grpc_name(&self) -> &'static str {
        self.grpc_name.unwrap_or(self.name)
    }
}

/// Compact field constructor.
const fn f(name: &'static str, kind: WireKind, doc: &'static str) -> WireField {
    WireField {
        name,
        kind,
        required: false,
        doc,
        surfaces: WireSurfaces::ALL,
        grpc_name: None,
    }
}

const fn req(name: &'static str, kind: WireKind, doc: &'static str) -> WireField {
    WireField {
        name,
        kind,
        required: true,
        doc,
        surfaces: WireSurfaces::ALL,
        grpc_name: None,
    }
}

const fn on(surfaces: WireSurfaces, field: WireField) -> WireField {
    WireField { surfaces, ..field }
}

/// One operation's wire description.
pub struct OpWire {
    /// Canonical operation name (matches `Operation::NAME` / the MCP tool
    /// prefix).
    pub op: &'static str,
    pub fields: &'static [WireField],
}

// ── Shared field texts (identical wording across ops keeps the golden
// diff against the previous hand-written schemas empty) ─────────────────

const WHERE_DOC: &str = "Filter conditions. Keys are field names, values are filter objects (e.g. {\"equals\": \"value\"}, {\"contains\": \"text\"}, {\"greater_than\": 5})";
const LOCALE_READ_DOC: &str = "Locale code (e.g. 'en', 'de') or 'all' for all locales";
const LOCALE_WRITE_DOC: &str = "Locale code (e.g. 'en', 'de') for localized fields";
const SELECT_DOC: &str = "Field names to return (projection); omit for all fields";
const DEPTH_DOC: &str = "Relationship population depth";
const SEARCH_DOC: &str = "Full-text search query";
const HOOKS_DOC: &str = "Run per-document lifecycle hooks (default: true)";
const EVENT_SINGLE_DOC: &str = "Emit a live-update event for this change (default: true)";

/// The collection operations' wire options.
pub static COLLECTION_OPS: &[OpWire] = &[
    OpWire {
        op: "find",
        fields: &[
            f("where", WireKind::FilterMap, WHERE_DOC),
            f(
                "order_by",
                WireKind::Str,
                "Sort field (prefix with - for descending). '_rank' (only together with 'search', page/offset pagination) sorts by search relevance, best first.",
            ),
            f("limit", WireKind::Int, "Max results per page"),
            f(
                "page",
                WireKind::Int,
                "Page number (1-indexed, page mode only)",
            ),
            on(
                WireSurfaces::LUA_ONLY,
                f("offset", WireKind::Int, "Number of results to skip"),
            ),
            f(
                "after_cursor",
                WireKind::Str,
                "Forward cursor (cursor mode only, mutually exclusive with page and before_cursor)",
            ),
            f(
                "before_cursor",
                WireKind::Str,
                "Backward cursor (cursor mode only, mutually exclusive with page and after_cursor)",
            ),
            f("depth", WireKind::Int, DEPTH_DOC),
            f("search", WireKind::Str, SEARCH_DOC),
            f("locale", WireKind::Locale, LOCALE_READ_DOC),
            f(
                "draft",
                WireKind::Bool,
                "When true, include draft documents (published + draft union)",
            ),
            f(
                "trash",
                WireKind::Bool,
                "When true, return only soft-deleted documents (trash view)",
            ),
            f("select", WireKind::Select, SELECT_DOC),
        ],
    },
    OpWire {
        op: "find_by_id",
        fields: &[
            req("id", WireKind::Id, ""),
            f("depth", WireKind::Int, DEPTH_DOC),
            f("locale", WireKind::Locale, LOCALE_READ_DOC),
            f(
                "draft",
                WireKind::Bool,
                "When true, overlay the latest draft version (draft view)",
            ),
            f(
                "trash",
                WireKind::Bool,
                "When true, look up among soft-deleted documents (trash view)",
            ),
            f("select", WireKind::Select, SELECT_DOC),
        ],
    },
    OpWire {
        op: "count",
        fields: &[
            f("where", WireKind::FilterMap, WHERE_DOC),
            f("search", WireKind::Str, SEARCH_DOC),
            f("locale", WireKind::Locale, LOCALE_READ_DOC),
            f(
                "draft",
                WireKind::Bool,
                "When true, include draft documents in the count (published + draft union)",
            ),
            f(
                "trash",
                WireKind::Bool,
                "When true, count only soft-deleted documents (trash view)",
            ),
        ],
    },
    OpWire {
        op: "create",
        fields: &[
            req("data", WireKind::DataFields, ""),
            f("locale", WireKind::Locale, LOCALE_WRITE_DOC),
            f(
                "draft",
                WireKind::Bool,
                "Write as a draft version (default: false)",
            ),
            on(
                WireSurfaces::LUA_ONLY,
                f("hooks", WireKind::Bool, HOOKS_DOC),
            ),
            f("events", WireKind::Bool, EVENT_SINGLE_DOC),
        ],
    },
    OpWire {
        op: "update",
        fields: &[
            req("id", WireKind::Id, ""),
            req("data", WireKind::DataFields, ""),
            f("locale", WireKind::Locale, LOCALE_WRITE_DOC),
            f(
                "draft",
                WireKind::Bool,
                "Write as a draft version (default: false)",
            ),
            on(
                WireSurfaces::LUA_ONLY,
                f("hooks", WireKind::Bool, HOOKS_DOC),
            ),
            // MCP spells unpublish only as the separate `unpublish` op;
            // gRPC and Lua also take it as an update flag.
            on(
                WireSurfaces::GRPC_LUA,
                f(
                    "unpublish",
                    WireKind::Bool,
                    "Transition a published document back to draft without changing field data",
                ),
            ),
            f("events", WireKind::Bool, EVENT_SINGLE_DOC),
        ],
    },
    OpWire {
        op: "validate",
        fields: &[
            f(
                "id",
                WireKind::Id,
                "Document ID — when set, validates as an update (excludes this row from unique checks)",
            ),
            req("data", WireKind::DataFields, ""),
            f("locale", WireKind::Locale, LOCALE_WRITE_DOC),
            f(
                "draft",
                WireKind::Bool,
                "Validate as a draft version (default: false)",
            ),
        ],
    },
    OpWire {
        op: "delete",
        fields: &[
            req("id", WireKind::Id, ""),
            f(
                "force_hard_delete",
                WireKind::Bool,
                "Bypass soft-delete and remove the row permanently (default: false)",
            ),
            on(
                WireSurfaces::LUA_ONLY,
                f("hooks", WireKind::Bool, HOOKS_DOC),
            ),
            f("events", WireKind::Bool, EVENT_SINGLE_DOC),
        ],
    },
    OpWire {
        op: "undelete",
        fields: &[
            req("id", WireKind::Id, ""),
            f("events", WireKind::Bool, EVENT_SINGLE_DOC),
        ],
    },
    OpWire {
        op: "unpublish",
        fields: &[
            req("id", WireKind::Id, ""),
            on(
                WireSurfaces::LUA_ONLY,
                f("hooks", WireKind::Bool, HOOKS_DOC),
            ),
            f("events", WireKind::Bool, EVENT_SINGLE_DOC),
        ],
    },
    OpWire {
        op: "create_many",
        fields: &[
            req(
                "documents",
                WireKind::DocumentsArray,
                "Array of documents to create",
            ),
            f("locale", WireKind::Locale, LOCALE_WRITE_DOC),
            f(
                "draft",
                WireKind::Bool,
                "Create documents as drafts (default: false)",
            ),
            f("hooks", WireKind::Bool, HOOKS_DOC),
            f(
                "events",
                WireKind::Bool,
                "Emit a live-update event per created document (default: false — bulk ops are quiet)",
            ),
        ],
    },
    OpWire {
        op: "update_many",
        fields: &[
            f("where", WireKind::FilterMap, WHERE_DOC),
            req(
                "data",
                WireKind::DataObject,
                "Field values to set on all matching documents",
            ),
            f("hooks", WireKind::Bool, HOOKS_DOC),
            f(
                "draft",
                WireKind::Bool,
                "Target draft versions (default: false)",
            ),
            f("locale", WireKind::Locale, LOCALE_WRITE_DOC),
            f(
                "events",
                WireKind::Bool,
                "Emit a live-update event per modified document (default: false — bulk ops are quiet)",
            ),
        ],
    },
    OpWire {
        op: "delete_many",
        fields: &[
            f(
                "where",
                WireKind::FilterMap,
                "Filter conditions. Keys are field names, values are filter objects (e.g. {\"equals\": \"value\"}, {\"contains\": \"text\"}). Omit to match all documents.",
            ),
            f("hooks", WireKind::Bool, HOOKS_DOC),
            on(
                WireSurfaces::LUA_ONLY,
                f(
                    "locale",
                    WireKind::Locale,
                    "Locale code. Validated but not used for matching (delete_many spans locales)",
                ),
            ),
            f(
                "force_hard_delete",
                WireKind::Bool,
                "Force hard delete even on soft-delete collections (default: false)",
            ),
            // `trash` (empty-the-trash) is deliberately NOT on the gRPC/MCP
            // wire yet — Lua + the admin empty-trash codec expose it.
            on(
                WireSurfaces::LUA_ONLY,
                f(
                    "trash",
                    WireKind::Bool,
                    "Target already-trashed documents and permanently remove them (empty the trash)",
                ),
            ),
            f(
                "events",
                WireKind::Bool,
                "Emit a live-update event per deleted document (default: false — bulk ops are quiet)",
            ),
        ],
    },
    OpWire {
        op: "list_versions",
        fields: &[
            req("id", WireKind::Id, "Document ID to list versions for"),
            f("limit", WireKind::Int, "Max versions to return"),
            f("offset", WireKind::Int, "Number of versions to skip"),
        ],
    },
    OpWire {
        op: "restore_version",
        fields: &[
            WireField {
                grpc_name: Some("document_id"),
                ..req("id", WireKind::Id, "Document ID to restore")
            },
            req(
                "version_id",
                WireKind::Str,
                "Version snapshot ID to restore from",
            ),
        ],
    },
];

/// The global operations' wire options.
pub static GLOBAL_OPS: &[OpWire] = &[
    OpWire {
        op: "get_global",
        fields: &[
            f("locale", WireKind::Locale, LOCALE_WRITE_DOC),
            f(
                "draft",
                WireKind::Bool,
                "Read unpublished (draft) content (default: false)",
            ),
        ],
    },
    OpWire {
        op: "update_global",
        fields: &[
            req("data", WireKind::DataFields, ""),
            f("locale", WireKind::Locale, LOCALE_WRITE_DOC),
            f(
                "draft",
                WireKind::Bool,
                "Write as a draft version (default: false)",
            ),
            on(
                WireSurfaces::LUA_ONLY,
                f("hooks", WireKind::Bool, HOOKS_DOC),
            ),
            f("events", WireKind::Bool, EVENT_SINGLE_DOC),
        ],
    },
    OpWire {
        op: "validate_global",
        fields: &[
            req("data", WireKind::DataFields, ""),
            f("locale", WireKind::Locale, LOCALE_WRITE_DOC),
            f(
                "draft",
                WireKind::Bool,
                "Validate as a draft version (default: false)",
            ),
        ],
    },
];

/// Look up a collection op's wire by name.
#[must_use]
pub fn collection_op(op: &str) -> Option<&'static OpWire> {
    COLLECTION_OPS.iter().find(|w| w.op == op)
}

/// Look up a global op's wire by name.
#[must_use]
pub fn global_op(op: &str) -> Option<&'static OpWire> {
    GLOBAL_OPS.iter().find(|w| w.op == op)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_ops() -> impl Iterator<Item = &'static OpWire> {
        COLLECTION_OPS.iter().chain(GLOBAL_OPS.iter())
    }

    #[test]
    fn op_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for w in all_ops() {
            assert!(seen.insert(w.op), "duplicate op name `{}`", w.op);
        }
    }

    #[test]
    fn field_names_unique_within_each_op() {
        for w in all_ops() {
            let mut seen = std::collections::HashSet::new();
            for f in w.fields {
                assert!(
                    seen.insert(f.name),
                    "op `{}`: duplicate field `{}`",
                    w.op,
                    f.name
                );
            }
        }
    }

    /// At most one top-level data spread per op — two would collide in the
    /// rendered object schema.
    #[test]
    fn at_most_one_data_spread_per_op() {
        for w in all_ops() {
            let spreads = w
                .fields
                .iter()
                .filter(|f| f.kind == WireKind::DataFields)
                .count();
            assert!(
                spreads <= 1,
                "op `{}` declares {spreads} DataFields spreads",
                w.op
            );
        }
    }

    /// Every field is exposed on at least one surface — a zero-surface field
    /// is dead weight in the model.
    #[test]
    fn every_field_has_a_surface() {
        for w in all_ops() {
            for f in w.fields {
                assert!(
                    f.surfaces.contains(WireSurfaces::GRPC)
                        || f.surfaces.contains(WireSurfaces::MCP)
                        || f.surfaces.contains(WireSurfaces::LUA),
                    "op `{}` field `{}` has no surface",
                    w.op,
                    f.name
                );
            }
        }
    }
}
