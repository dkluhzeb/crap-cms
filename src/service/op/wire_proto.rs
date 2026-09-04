//! Proto generation: the CRUD request messages of `proto/content.proto`
//! rendered from the single-source wire model — `cargo xtask gen-proto`.
//!
//! [`PROTO_MESSAGES`] pins, per operation, every proto field's wire spelling,
//! type, TAG, and doc comment. [`regenerate_proto`] splices freshly rendered
//! message bodies into the proto text; everything outside these messages
//! (responses, auth, jobs, subscribe, the service block) stays hand-written
//! and untouched. CI gates on `cargo xtask gen-proto --check`, and the
//! in-crate sync test below fails whenever the committed file diverges.
//!
//! Freeze rules this construction enforces:
//! - a field added to the wire model without a [`ProtoField`] entry (or vice
//!   versa) fails the wire-parity test (`tests/wire_parity.rs`);
//! - a shipped tag can never be renumbered or retyped silently — the file is
//!   regenerated from the pinned entries, and `--check` diffs it;
//! - tags are append-only: remove a field only by reserving its tag.

/// One pinned proto field: wire spelling, proto type, tag, and the exact
/// comment block rendered above it (lines joined by `\n`; empty = none).
pub struct ProtoField {
    pub name: &'static str,
    pub ty: &'static str,
    pub tag: u16,
    pub doc: &'static str,
}

/// One generated request message.
pub struct ProtoMessage {
    /// Canonical operation name (matches `wire::OpWire::op`).
    pub op: &'static str,
    /// Proto message name.
    pub message: &'static str,
    /// Fields in declaration order, routing field (`collection`/`slug`,
    /// always tag 1) included.
    pub fields: &'static [ProtoField],
}

pub static PROTO_MESSAGES: &[ProtoMessage] = &[
    ProtoMessage {
        op: "find",
        message: "FindRequest",
        fields: &[
            ProtoField {
                name: "collection",
                ty: "string",
                tag: 1,
                doc: "The collection slug (as defined in the Lua collection definition).",
            },
            ProtoField {
                name: "where",
                ty: "optional string",
                tag: 2,
                doc: "Optional JSON filter clause. The filter is a JSON object where keys are\nfield names and values are either a string (shorthand equals) or an object\nwith an operator key.\n\nSupported operators:\n  equals              — exact match (also the shorthand: \"field\": \"value\")\n  not_equals          — not equal\n  like                — SQL LIKE pattern (use % as wildcard)\n  contains            — substring match (wraps value in %...%)\n  greater_than        — numeric or string greater-than\n  less_than           — numeric or string less-than\n  greater_than_or_equal\n  less_than_or_equal\n  in                  — value must be an array: {\"status\": {\"in\": [\"a\",\"b\"]}}\n  not_in              — value must be an array\n  exists              — field is not NULL\n  not_exists          — field is NULL\n\nOR groups:\n  {\"or\": [{\"status\": \"active\"}, {\"status\": \"pending\"}]}\n\nMultiple top-level keys are ANDed together.\nDot notation for nested fields: \"group.field\" or \"block.field\".\n\nExamples:\n  {\"status\": \"published\"}\n  {\"age\": {\"greater_than\": \"18\"}}\n  {\"tags\": {\"in\": [\"rust\", \"cms\"]}}\n  {\"or\": [{\"role\": \"admin\"}, {\"role\": \"editor\"}]}",
            },
            ProtoField {
                name: "order_by",
                ty: "optional string",
                tag: 3,
                doc: "Sort expression. Prefix with `-` for descending order.\nDefaults to created_at DESC (for timestamped collections) or id ASC.\n\nExamples:  \"title\"  (ascending),  \"-created_at\"  (newest first)",
            },
            ProtoField {
                name: "limit",
                ty: "optional int64",
                tag: 4,
                doc: "Maximum number of documents to return per page.\nClamped to server-configured max_limit (default 1000).\nDefaults to server-configured default_limit (default 20).",
            },
            ProtoField {
                name: "page",
                ty: "optional int64",
                tag: 5,
                doc: "1-indexed page number for page-based pagination.\nMutually exclusive with after_cursor and before_cursor.\nDefaults to 1 when neither page nor cursors are specified.",
            },
            ProtoField {
                name: "depth",
                ty: "optional int32",
                tag: 6,
                doc: "Relationship population depth. 0 = IDs only.\n1 = populate immediate relationship fields with full documents.\nDefaults to the server's default_depth config (1 unless changed).\nHigher values recurse further; capped by server max_depth config.\nSee crap.toml [depth] section.",
            },
            ProtoField {
                name: "locale",
                ty: "optional string",
                tag: 7,
                doc: "BCP-47 locale code for localized fields (e.g., \"en\", \"fr\", \"pt-BR\").\nFalls back to the configured default locale when omitted.",
            },
            ProtoField {
                name: "select",
                ty: "repeated string",
                tag: 8,
                doc: "Field projection: only return the listed field names.\nEmpty list returns all fields (default).",
            },
            ProtoField {
                name: "draft",
                ty: "optional bool",
                tag: 9,
                doc: "When true, includes draft documents (requires drafts enabled on collection).\nDefaults to false — only published documents are returned.",
            },
            ProtoField {
                name: "after_cursor",
                ty: "optional string",
                tag: 10,
                doc: "Opaque cursor for forward pagination (page to the next page).\nObtain from PaginationInfo.end_cursor in a previous response.\nMutually exclusive with before_cursor and page.\nOnly effective when cursor pagination is enabled server-side.",
            },
            ProtoField {
                name: "before_cursor",
                ty: "optional string",
                tag: 11,
                doc: "Opaque cursor for backward pagination (page to the previous page).\nObtain from PaginationInfo.start_cursor in a previous response.\nMutually exclusive with after_cursor and page.\nOnly effective when cursor pagination is enabled server-side.",
            },
            ProtoField {
                name: "search",
                ty: "optional string",
                tag: 12,
                doc: "Full-text search query string. When set, results are ranked by relevance\nagainst the collection's full-text search index.",
            },
            ProtoField {
                name: "trash",
                ty: "optional bool",
                tag: 13,
                doc: "When true, returns only soft-deleted documents (trash view).\nOnly effective on collections with `soft_delete = true`.\nAuth: `access.trash` is evaluated instead of `access.read`.\nDefaults to false — soft-deleted documents are excluded.",
            },
        ],
    },
    ProtoMessage {
        op: "find_by_id",
        message: "FindByIDRequest",
        fields: &[
            ProtoField {
                name: "collection",
                ty: "string",
                tag: 1,
                doc: "The collection slug.",
            },
            ProtoField {
                name: "id",
                ty: "string",
                tag: 2,
                doc: "The document's nanoid ID.",
            },
            ProtoField {
                name: "depth",
                ty: "optional int32",
                tag: 3,
                doc: "Relationship population depth. 0 = IDs only.\nDefaults to the server's default_depth config (1 unless changed) — the\nsame default as Find.",
            },
            ProtoField {
                name: "locale",
                ty: "optional string",
                tag: 4,
                doc: "BCP-47 locale code for localized fields.",
            },
            ProtoField {
                name: "select",
                ty: "repeated string",
                tag: 5,
                doc: "Field projection: only return the listed field names. Empty = all fields.",
            },
            ProtoField {
                name: "draft",
                ty: "optional bool",
                tag: 6,
                doc: "When true, loads the latest draft version snapshot instead of the published\ndocument (only applicable to collections with drafts and versions enabled).",
            },
            ProtoField {
                name: "trash",
                ty: "optional bool",
                tag: 7,
                doc: "When true, allows finding a soft-deleted document by ID.\nOnly effective on collections with `soft_delete = true`.\nAuth: `access.trash` is evaluated (falls back to `access.update`).",
            },
        ],
    },
    ProtoMessage {
        op: "count",
        message: "CountRequest",
        fields: &[
            ProtoField {
                name: "collection",
                ty: "string",
                tag: 1,
                doc: "The collection slug.",
            },
            ProtoField {
                name: "where",
                ty: "optional string",
                tag: 2,
                doc: "Optional JSON filter clause. Same syntax as FindRequest.where.",
            },
            ProtoField {
                name: "locale",
                ty: "optional string",
                tag: 3,
                doc: "BCP-47 locale for localized field filtering.",
            },
            ProtoField {
                name: "draft",
                ty: "optional bool",
                tag: 4,
                doc: "When true, includes draft documents in the count. Defaults to false.",
            },
            ProtoField {
                name: "search",
                ty: "optional string",
                tag: 5,
                doc: "Full-text search query. When set, only documents matching the search\nare counted.",
            },
            ProtoField {
                name: "trash",
                ty: "optional bool",
                tag: 6,
                doc: "When true, counts soft-deleted (trashed) documents instead of live\nones — mirrors FindRequest.trash. Defaults to false.",
            },
        ],
    },
    ProtoMessage {
        op: "create",
        message: "CreateRequest",
        fields: &[
            ProtoField {
                name: "collection",
                ty: "string",
                tag: 1,
                doc: "The collection slug.",
            },
            ProtoField {
                name: "data",
                ty: "DataMap",
                tag: 2,
                doc: "Document field values as a protobuf Struct (JSON-compatible key/value map).\nFor auth collections, include a \"password\" key; it is extracted before\nhooks run and stored as an Argon2id hash in a hidden column.",
            },
            ProtoField {
                name: "locale",
                ty: "optional string",
                tag: 3,
                doc: "BCP-47 locale for localized field writes.",
            },
            ProtoField {
                name: "draft",
                ty: "optional bool",
                tag: 4,
                doc: "When true, saves the document as a draft (status=draft) rather than\npublishing it immediately. Only applies when drafts are enabled.",
            },
            ProtoField {
                name: "events",
                ty: "optional bool",
                tag: 5,
                doc: "Emit a live-update event for the created document. Default: true.\nSet false for a quiet write (e.g. seeding/migrations).",
            },
        ],
    },
    ProtoMessage {
        op: "update",
        message: "UpdateRequest",
        fields: &[
            ProtoField {
                name: "collection",
                ty: "string",
                tag: 1,
                doc: "The collection slug.",
            },
            ProtoField {
                name: "id",
                ty: "string",
                tag: 2,
                doc: "The document's nanoid ID.",
            },
            ProtoField {
                name: "data",
                ty: "DataMap",
                tag: 3,
                doc: "Updated field values. Only provided fields are written; others are unchanged\n(partial update semantics). For auth collections, \"password\" is handled\nspecially — empty string means keep the existing password unchanged.",
            },
            ProtoField {
                name: "locale",
                ty: "optional string",
                tag: 4,
                doc: "BCP-47 locale for localized field writes.",
            },
            ProtoField {
                name: "draft",
                ty: "optional bool",
                tag: 5,
                doc: "When true, saves changes as a draft version (does not publish).",
            },
            ProtoField {
                name: "unpublish",
                ty: "optional bool",
                tag: 6,
                doc: "When true, transitions a published document back to draft status without\nmodifying field data. Ignored if the collection does not have versions enabled.",
            },
            ProtoField {
                name: "events",
                ty: "optional bool",
                tag: 7,
                doc: "Emit a live-update event for the updated document. Default: true.\nSet false for a quiet write.",
            },
        ],
    },
    ProtoMessage {
        op: "validate",
        message: "ValidateRequest",
        fields: &[
            ProtoField {
                name: "collection",
                ty: "string",
                tag: 1,
                doc: "The collection slug.",
            },
            ProtoField {
                name: "data",
                ty: "DataMap",
                tag: 2,
                doc: "Document field values to validate (same format as CreateRequest.data).",
            },
            ProtoField {
                name: "draft",
                ty: "optional bool",
                tag: 3,
                doc: "When true, validate as a draft (relaxes required-field checks for\ncollections with drafts enabled).",
            },
            ProtoField {
                name: "locale",
                ty: "optional string",
                tag: 4,
                doc: "BCP-47 locale code for localized field validation.",
            },
            ProtoField {
                name: "id",
                ty: "optional string",
                tag: 5,
                doc: "When set, exclude this document ID from unique-field checks (update path).\nOmit for create validation.",
            },
        ],
    },
    ProtoMessage {
        op: "delete",
        message: "DeleteRequest",
        fields: &[
            ProtoField {
                name: "collection",
                ty: "string",
                tag: 1,
                doc: "The collection slug.",
            },
            ProtoField {
                name: "id",
                ty: "string",
                tag: 2,
                doc: "The document's nanoid ID.",
            },
            ProtoField {
                name: "force_hard_delete",
                ty: "bool",
                tag: 3,
                doc: "When true, permanently delete even if the collection has soft_delete enabled.\nRequires `access.delete` permission (not just `access.trash`).",
            },
            ProtoField {
                name: "events",
                ty: "optional bool",
                tag: 4,
                doc: "Emit a live-update event for the deleted document. Default: true.\nSet false for a quiet delete.",
            },
        ],
    },
    ProtoMessage {
        op: "undelete",
        message: "UndeleteRequest",
        fields: &[
            ProtoField {
                name: "collection",
                ty: "string",
                tag: 1,
                doc: "The collection slug.",
            },
            ProtoField {
                name: "id",
                ty: "string",
                tag: 2,
                doc: "The document's nanoid ID.",
            },
            ProtoField {
                name: "events",
                ty: "optional bool",
                tag: 3,
                doc: "Emit a live-update event for the restored document. Default: true.\nSet false for a quiet restore.",
            },
        ],
    },
    ProtoMessage {
        op: "create_many",
        message: "CreateManyRequest",
        fields: &[
            ProtoField {
                name: "collection",
                ty: "string",
                tag: 1,
                doc: "The collection slug.",
            },
            ProtoField {
                name: "documents",
                ty: "repeated DataMap",
                tag: 2,
                doc: "List of documents to create. Each item is a protobuf Struct with field values.",
            },
            ProtoField {
                name: "locale",
                ty: "optional string",
                tag: 3,
                doc: "BCP-47 locale for localized field writes.",
            },
            ProtoField {
                name: "draft",
                ty: "optional bool",
                tag: 4,
                doc: "When true, saves documents as drafts. Defaults to false.",
            },
            ProtoField {
                name: "hooks",
                ty: "optional bool",
                tag: 5,
                doc: "Run per-document lifecycle hooks.\nDefault: true. Set to false to skip for performance.",
            },
            ProtoField {
                name: "events",
                ty: "optional bool",
                tag: 6,
                doc: "Emit a live-update event per created document. Default: false (bulk\noperations are quiet). Set true to notify event-stream subscribers.",
            },
            ProtoField {
                name: "queue",
                ty: "optional bool",
                tag: 7,
                doc: "Run as a queued background job instead of synchronously. The response\ncarries only job_id and the work runs later under the caller's identity;\npoll GetJobRun with that id for status and the result summary.\nDefaults to false (run synchronously).",
            },
        ],
    },
    ProtoMessage {
        op: "update_many",
        message: "UpdateManyRequest",
        fields: &[
            ProtoField {
                name: "collection",
                ty: "string",
                tag: 1,
                doc: "The collection slug.",
            },
            ProtoField {
                name: "where",
                ty: "optional string",
                tag: 2,
                doc: "Optional JSON filter clause. Same syntax as FindRequest.where.\nWhen omitted, all documents in the collection are updated.",
            },
            ProtoField {
                name: "data",
                ty: "DataMap",
                tag: 3,
                doc: "The field values to apply to all matching documents (partial update).",
            },
            ProtoField {
                name: "locale",
                ty: "optional string",
                tag: 4,
                doc: "BCP-47 locale for localized field writes.",
            },
            ProtoField {
                name: "draft",
                ty: "optional bool",
                tag: 5,
                doc: "When true, saves changes as drafts. Defaults to false.",
            },
            ProtoField {
                name: "hooks",
                ty: "optional bool",
                tag: 6,
                doc: "Run per-document lifecycle hooks (before_change, after_change).\nDefault: true (hooks run). Set to false to skip for performance.",
            },
            ProtoField {
                name: "events",
                ty: "optional bool",
                tag: 7,
                doc: "Emit a live-update event per modified document. Default: false (bulk\noperations are quiet). Set true to notify event-stream subscribers.",
            },
            ProtoField {
                name: "queue",
                ty: "optional bool",
                tag: 8,
                doc: "Run as a queued background job instead of synchronously. The response\ncarries only job_id and the work runs later under the caller's identity;\npoll GetJobRun with that id for status and the result summary.\nDefaults to false (run synchronously).",
            },
        ],
    },
    ProtoMessage {
        op: "delete_many",
        message: "DeleteManyRequest",
        fields: &[
            ProtoField {
                name: "collection",
                ty: "string",
                tag: 1,
                doc: "The collection slug.",
            },
            ProtoField {
                name: "where",
                ty: "optional string",
                tag: 2,
                doc: "Optional JSON filter clause. Same syntax as FindRequest.where.\nWhen omitted, all documents in the collection are deleted.",
            },
            ProtoField {
                name: "hooks",
                ty: "optional bool",
                tag: 3,
                doc: "Run per-document lifecycle hooks (before_delete, after_delete).\nDefault: true (hooks run). Set to false to skip for performance.",
            },
            ProtoField {
                name: "force_hard_delete",
                ty: "bool",
                tag: 4,
                doc: "When true, permanently delete even if the collection has soft_delete enabled.\nRequires `access.delete` permission (not just `access.trash`).",
            },
            ProtoField {
                name: "events",
                ty: "optional bool",
                tag: 5,
                doc: "Emit a live-update event per deleted document. Default: false (bulk\noperations are quiet). Set true to notify event-stream subscribers.",
            },
            ProtoField {
                name: "queue",
                ty: "optional bool",
                tag: 6,
                doc: "Run as a queued background job instead of synchronously. The response\ncarries only job_id and the work runs later under the caller's identity;\npoll GetJobRun with that id for status and the result summary.\nDefaults to false (run synchronously).",
            },
        ],
    },
    ProtoMessage {
        op: "list_versions",
        message: "ListVersionsRequest",
        fields: &[
            ProtoField {
                name: "collection",
                ty: "string",
                tag: 1,
                doc: "The collection slug (must have versioning/drafts enabled).",
            },
            ProtoField {
                name: "id",
                ty: "string",
                tag: 2,
                doc: "The document's nanoid ID.",
            },
            ProtoField {
                name: "limit",
                ty: "optional int64",
                tag: 3,
                doc: "Maximum number of versions to return. Defaults to all versions when absent.",
            },
            ProtoField {
                name: "offset",
                ty: "optional int64",
                tag: 4,
                doc: "Number of versions to skip (pagination offset). Defaults to 0.",
            },
        ],
    },
    ProtoMessage {
        op: "restore_version",
        message: "RestoreVersionRequest",
        fields: &[
            ProtoField {
                name: "collection",
                ty: "string",
                tag: 1,
                doc: "The collection slug (must have versioning enabled).",
            },
            ProtoField {
                name: "document_id",
                ty: "string",
                tag: 2,
                doc: "The nanoid ID of the document to restore.",
            },
            ProtoField {
                name: "version_id",
                ty: "string",
                tag: 3,
                doc: "The nanoid ID of the version to restore from (from VersionInfo.id).",
            },
        ],
    },
    ProtoMessage {
        op: "get_global",
        message: "GetGlobalRequest",
        fields: &[
            ProtoField {
                name: "slug",
                ty: "string",
                tag: 1,
                doc: "The global slug (as defined in the Lua global definition).",
            },
            ProtoField {
                name: "locale",
                ty: "optional string",
                tag: 2,
                doc: "BCP-47 locale for localized field reads.",
            },
            ProtoField {
                name: "draft",
                ty: "optional bool",
                tag: 3,
                doc: "When true, reads unpublished (draft) content for a global that has drafts\nenabled and has been unpublished. Default false serves the last published\nsnapshot. Subject to the global's read access.",
            },
        ],
    },
    ProtoMessage {
        op: "update_global",
        message: "UpdateGlobalRequest",
        fields: &[
            ProtoField {
                name: "slug",
                ty: "string",
                tag: 1,
                doc: "The global slug.",
            },
            ProtoField {
                name: "data",
                ty: "DataMap",
                tag: 2,
                doc: "Updated field values (partial update semantics).",
            },
            ProtoField {
                name: "locale",
                ty: "optional string",
                tag: 3,
                doc: "BCP-47 locale for localized field writes.",
            },
            ProtoField {
                name: "events",
                ty: "optional bool",
                tag: 4,
                doc: "Emit a live-update event for the updated global. Default: true.\nSet false for a quiet write.",
            },
            ProtoField {
                name: "draft",
                ty: "optional bool",
                tag: 5,
                doc: "Save as an unpublished draft (drafts-enabled globals only). Default:\nfalse (publishes). Parity with the MCP/Lua/admin global update.",
            },
        ],
    },
    ProtoMessage {
        op: "validate_global",
        message: "ValidateGlobalRequest",
        fields: &[
            ProtoField {
                name: "slug",
                ty: "string",
                tag: 1,
                doc: "The global slug.",
            },
            ProtoField {
                name: "data",
                ty: "DataMap",
                tag: 2,
                doc: "Global field values to validate (same format as UpdateGlobalRequest.data).",
            },
            ProtoField {
                name: "draft",
                ty: "optional bool",
                tag: 3,
                doc: "When true, validate as a draft (relaxes required-field checks for\nglobals with drafts enabled).",
            },
            ProtoField {
                name: "locale",
                ty: "optional string",
                tag: 4,
                doc: "BCP-47 locale code for localized field validation.",
            },
        ],
    },
];

/// The pinned proto spec for one op, or `None` for ops with no gRPC message
/// of their own (`unpublish` — gRPC spells it as `UpdateRequest.unpublish`).
#[must_use]
pub fn proto_message(op: &str) -> Option<&'static ProtoMessage> {
    PROTO_MESSAGES.iter().find(|m| m.op == op)
}

/// Render one message's body (the text between `message X {` and `}`).
fn render_body(msg: &ProtoMessage) -> String {
    let mut blocks = Vec::with_capacity(msg.fields.len());

    for field in msg.fields {
        let mut lines = Vec::new();
        if !field.doc.is_empty() {
            for line in field.doc.split('\n') {
                if line.is_empty() {
                    lines.push("  //".to_string());
                } else {
                    lines.push(format!("  // {line}"));
                }
            }
        }
        lines.push(format!("  {} {} = {};", field.ty, field.name, field.tag));
        blocks.push(lines.join("\n"));
    }

    let mut body = blocks.join("\n\n");
    body.push('\n');
    body
}

/// Regenerate the proto text: every message in [`PROTO_MESSAGES`] gets its
/// body re-rendered from the pinned spec; the rest of `src` passes through
/// unchanged. Idempotent — running it on an in-sync file is a no-op.
///
/// # Panics
///
/// Panics when a pinned message is missing from `src` — the spec and the
/// proto file must always describe the same set of messages.
#[must_use]
pub fn regenerate_proto(src: &str) -> String {
    let mut out = src.to_string();

    for msg in PROTO_MESSAGES {
        let header = format!("message {} {{\n", msg.message);
        let start = out
            .find(&header)
            .unwrap_or_else(|| panic!("proto message `{}` not found", msg.message));
        let body_start = start + header.len();
        let body_end = out[body_start..].find("\n}").map_or_else(
            || panic!("proto message `{}` has no closing brace", msg.message),
            |i| body_start + i + 1,
        );

        out.replace_range(body_start..body_end, &render_body(msg));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROTO: &str = include_str!("../../../proto/content.proto");

    /// The committed proto must match what the pinned spec renders — mirrors
    /// the `cargo xtask gen-proto --check` gate so plain `cargo test`
    /// catches drift.
    #[test]
    fn proto_file_is_in_sync() {
        assert_eq!(
            regenerate_proto(PROTO),
            PROTO,
            "proto/content.proto is stale — run `cargo xtask gen-proto` \
             (never hand-edit a generated message body)"
        );
    }

    /// Tags are unique within each message and the routing field is tag 1.
    #[test]
    fn tags_are_unique_and_routing_is_first() {
        for msg in PROTO_MESSAGES {
            let mut tags: Vec<u16> = msg.fields.iter().map(|f| f.tag).collect();
            tags.sort_unstable();
            let len = tags.len();
            tags.dedup();
            assert_eq!(tags.len(), len, "duplicate tag in `{}`", msg.message);

            let first = &msg.fields[0];
            assert_eq!(
                first.tag, 1,
                "`{}` routing field must be tag 1",
                msg.message
            );
            assert!(
                first.name == "collection" || first.name == "slug",
                "`{}` must start with its routing field",
                msg.message
            );
        }
    }
}
