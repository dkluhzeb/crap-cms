//! Wire-parity checker: diffs the remaining hand-maintained wire
//! descriptions — `proto/content.proto` and the generated-from-code
//! `types/crap.lua` — against the single-source wire model
//! (`crap_cms::service::op::wire`).
//!
//! The MCP schemas are *rendered* from the model, so they can't drift; the
//! proto file and the Lua option structs are still written by hand, so this
//! test is what turns "forgot the sibling surface" from a latent bug into a
//! red CI run. A failure names the op, the surface, and the missing/extra
//! field.

use std::collections::{BTreeMap, BTreeSet};

use crap_cms::service::op::{
    wire::{self, OpWire, WireField, WireKind, WireSurfaces},
    wire_proto::{self, expected_proto_ty},
};

const PROTO: &str = include_str!("../proto/content.proto");
const CRAP_LUA: &str = include_str!("../types/crap.lua");

// ── Proto side ──────────────────────────────────────────────────────────

/// Extract the field names of one proto message body.
fn proto_message_fields(message: &str) -> BTreeSet<String> {
    let header = format!("message {message} {{");
    let start = PROTO
        .find(&header)
        .unwrap_or_else(|| panic!("proto message `{message}` not found"));
    let body_start = start + header.len();
    let body_end = PROTO[body_start..].find("\n}").map_or_else(
        || panic!("proto message `{message}` has no closing brace"),
        |i| body_start + i,
    );

    let mut out = BTreeSet::new();
    for line in PROTO[body_start..body_end].lines() {
        let line = line.trim();
        if line.starts_with("//") || !line.ends_with(';') {
            continue;
        }
        // `[optional|repeated] <type> <name> = <tag>;`
        let Some(eq) = line.find('=') else { continue };
        let Some(name) = line[..eq].split_whitespace().last() else {
            continue;
        };
        out.insert(name.to_string());
    }
    out
}

/// The model's expected proto field names for one op (routing field included).
fn expected_proto_fields(w: &OpWire, routing: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    out.insert(routing.to_string());

    for field in w.fields {
        if !field.surfaces.contains(WireSurfaces::GRPC) {
            continue;
        }
        let name = match field.kind {
            WireKind::DataFields | WireKind::DataObject => "data",
            WireKind::DocumentsArray => "documents",
            _ => field.grpc_name(),
        };
        out.insert(name.to_string());
    }
    out
}

fn assert_same(op: &str, surface: &str, expected: &BTreeSet<String>, actual: &BTreeSet<String>) {
    let missing: Vec<_> = expected.difference(actual).collect();
    let extra: Vec<_> = actual.difference(expected).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "wire drift on `{op}` ({surface}): missing {missing:?}, undeclared {extra:?} — \
         update src/service/op/wire.rs and the {surface} description together"
    );
}

/// Every gRPC request message carries exactly the model's GRPC-surface
/// fields (plus its routing field). `unpublish` has no message of its own —
/// gRPC spells it as `UpdateRequest.unpublish`, which the model declares as
/// a GRPC/Lua-only field on `update`.
#[test]
fn proto_messages_match_wire_model() {
    let map: &[(&str, Option<&str>, &str)] = &[
        ("find", Some("FindRequest"), "collection"),
        ("find_by_id", Some("FindByIDRequest"), "collection"),
        ("count", Some("CountRequest"), "collection"),
        ("create", Some("CreateRequest"), "collection"),
        ("update", Some("UpdateRequest"), "collection"),
        ("validate", Some("ValidateRequest"), "collection"),
        ("delete", Some("DeleteRequest"), "collection"),
        ("undelete", Some("UndeleteRequest"), "collection"),
        ("unpublish", None, "collection"),
        ("create_many", Some("CreateManyRequest"), "collection"),
        ("update_many", Some("UpdateManyRequest"), "collection"),
        ("delete_many", Some("DeleteManyRequest"), "collection"),
        ("list_versions", Some("ListVersionsRequest"), "collection"),
        (
            "restore_version",
            Some("RestoreVersionRequest"),
            "collection",
        ),
    ];

    for (op, message, routing) in map {
        let Some(message) = message else { continue };
        let w = wire::collection_op(op).expect("wire model covers every op");
        assert_same(
            op,
            "proto",
            &expected_proto_fields(w, routing),
            &proto_message_fields(message),
        );
    }

    for (op, message) in [
        ("get_global", "GetGlobalRequest"),
        ("update_global", "UpdateGlobalRequest"),
        ("validate_global", "ValidateGlobalRequest"),
    ] {
        let w = wire::global_op(op).expect("wire model covers every global op");
        assert_same(
            op,
            "proto",
            &expected_proto_fields(w, "slug"),
            &proto_message_fields(message),
        );
    }
}

/// The map above must name every collection op the model declares — a new
/// op can't be added without deciding its proto story.
#[test]
fn proto_map_covers_every_model_op() {
    let mapped: BTreeSet<&str> = [
        "find",
        "find_by_id",
        "count",
        "create",
        "update",
        "validate",
        "delete",
        "undelete",
        "unpublish",
        "create_many",
        "update_many",
        "delete_many",
        "list_versions",
        "restore_version",
    ]
    .into_iter()
    .collect();

    for w in wire::COLLECTION_OPS {
        assert!(
            mapped.contains(w.op),
            "op `{}` missing from the proto parity map",
            w.op
        );
    }
}

// ── Proto spec (generator) side ─────────────────────────────────────────

/// Every GRPC-surface model field has exactly one entry in the pinned proto
/// spec (`wire_proto::PROTO_MESSAGES`) and vice versa — the wire model and
/// the proto generator can't drift apart. The routing field (tag 1) is
/// structural and excluded. `unpublish` has no message (gRPC spells it as
/// `UpdateRequest.unpublish`).
#[test]
fn proto_spec_covers_exactly_the_grpc_surface() {
    for w in wire::COLLECTION_OPS.iter().chain(wire::GLOBAL_OPS.iter()) {
        if w.op == "unpublish" {
            assert!(
                wire_proto::proto_message(w.op).is_none(),
                "unpublish must not have a proto message"
            );
            continue;
        }

        let msg = wire_proto::proto_message(w.op)
            .unwrap_or_else(|| panic!("no pinned proto message for op `{}`", w.op));
        let spec_names: BTreeSet<&str> = msg.fields[1..].iter().map(|f| f.name).collect();

        let model_names: BTreeSet<&str> = w
            .fields
            .iter()
            .filter(|f| f.surfaces.contains(WireSurfaces::GRPC))
            .map(|f| match f.kind {
                WireKind::DataFields | WireKind::DataObject => "data",
                WireKind::DocumentsArray => "documents",
                _ => f.grpc_name(),
            })
            .collect();

        assert_eq!(
            model_names, spec_names,
            "op `{}`: wire model GRPC surface and pinned proto spec diverge",
            w.op
        );
    }
}

/// The pinned proto spec's TYPES, not just its field names. `ProtoField::ty`
/// is a hand-typed string; [`expected_proto_ty`] derives the one spelling each
/// field's `WireKind` allows, so a `ty` that disagrees with the model is drift
/// and fails here.
///
/// The CRUD messages' tag-1 routing field (`collection`/`slug`) is structural,
/// not a modeled field, so it is skipped; on the job ops the identifying
/// argument IS modeled and takes part.
#[test]
fn proto_spec_types_match_the_wire_model() {
    for w in wire::COLLECTION_OPS.iter().chain(wire::GLOBAL_OPS.iter()) {
        assert_spec_types(w, 1);
    }

    for w in wire::JOB_OPS {
        assert_spec_types(w, 0);
    }
}

/// Compare one op's pinned proto types against the model, skipping the first
/// `structural` spec fields (the CRUD routing field).
fn assert_spec_types(w: &OpWire, structural: usize) {
    let Some(msg) = wire_proto::proto_message(w.op) else {
        return;
    };

    for field in w.fields {
        if !field.surfaces.contains(WireSurfaces::GRPC) {
            continue;
        }

        let name = match field.kind {
            WireKind::DataFields | WireKind::DataObject => "data",
            WireKind::DocumentsArray => "documents",
            _ => field.grpc_name(),
        };
        let Some(pinned) = msg.fields[structural..].iter().find(|f| f.name == name) else {
            panic!("op `{}`: no pinned proto field `{name}`", w.op);
        };

        assert_eq!(
            pinned.ty,
            expected_proto_ty(field),
            "op `{}` field `{name}`: pinned proto type disagrees with the wire model — \
             fix the pin in src/service/op/wire_proto.rs or the kind in wire.rs",
            w.op
        );
    }
}

// ── Lua side ────────────────────────────────────────────────────────────

/// The declared type on a `--- @field` line: everything up to the first space
/// outside angle brackets, since a `table<string, A | B[]>` annotation carries
/// spaces inside its type parameters.
fn lua_field_type(rest: &str) -> String {
    let mut depth = 0usize;
    let mut out = String::new();

    for c in rest.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            ' ' if depth == 0 => break,
            _ => {}
        }
        out.push(c);
    }

    out
}

/// The `@field name type` pairs of one `@class` block in `types/crap.lua`.
fn lua_class_typed_fields(class: &str) -> BTreeMap<String, String> {
    let header = format!("--- @class {class}\n");
    let start = CRAP_LUA
        .find(&header)
        .unwrap_or_else(|| panic!("Lua class `{class}` not found in types/crap.lua"));

    let mut out = BTreeMap::new();
    for line in CRAP_LUA[start + header.len()..].lines() {
        let Some(rest) = line.strip_prefix("--- @field ") else {
            break;
        };
        let Some(name) = rest.split_whitespace().next() else {
            break;
        };
        let ty = lua_field_type(rest[name.len()..].trim_start());
        out.insert(name.trim_end_matches('?').to_string(), ty);
    }
    out
}

/// Extract the `@field` names of one `@class` block in `types/crap.lua`.
fn lua_class_fields(class: &str) -> BTreeSet<String> {
    lua_class_typed_fields(class).into_keys().collect()
}

/// The Lua annotation spelling each wire kind takes in the generated option
/// classes. `None` for the kinds no option class carries — document data is
/// positional, and the job ops' payload/status/duration kinds live in tables
/// that are key-checked against the model rather than annotated per op.
fn expected_lua_ty(kind: WireKind) -> Option<&'static str> {
    Some(match kind {
        WireKind::Bool => "boolean",
        WireKind::Int | WireKind::Int32 => "integer",
        WireKind::Str | WireKind::Id | WireKind::Locale => "string",
        WireKind::Select => "string[]",
        WireKind::FilterMap => "table<string, crap.FilterValue | crap.OrCondition[]>",
        WireKind::DataFields
        | WireKind::DataObject
        | WireKind::DocumentsArray
        | WireKind::JsonData
        | WireKind::JobStatus
        | WireKind::Duration => return None,
    })
}

/// (op, Lua classes to union, positional args) for every collection op.
const COLLECTION_LUA: &[(&str, &[&str], &[&str])] = &[
    ("find", &["crap.FindQuery"], &[]),
    ("find_by_id", &["crap.FindByIdOptions"], &["id"]),
    ("count", &["crap.CountQuery"], &[]),
    ("create", &["crap.CreateOptions"], &[]),
    ("update", &["crap.UpdateOptions"], &["id"]),
    ("validate", &["crap.ValidateOptions"], &[]),
    ("delete", &["crap.DeleteOptions"], &["id"]),
    ("undelete", &["crap.UndeleteOptions"], &["id"]),
    ("unpublish", &["crap.UnpublishOptions"], &["id"]),
    ("create_many", &["crap.CreateManyOptions"], &[]),
    (
        "update_many",
        &["crap.UpdateManyQuery", "crap.UpdateManyOptions"],
        &[],
    ),
    (
        "delete_many",
        &["crap.DeleteManyQuery", "crap.DeleteManyOptions"],
        &[],
    ),
    ("list_versions", &["crap.ListVersionsOptions"], &["id"]),
    (
        "restore_version",
        &["crap.RestoreVersionOptions"],
        &["id", "version_id"],
    ),
];

/// (op, Lua options class) for every global op.
const GLOBAL_LUA: &[(&str, &str)] = &[
    ("get_global", "crap.GlobalGetOptions"),
    ("update_global", "crap.GlobalUpdateOptions"),
    ("validate_global", "crap.GlobalValidateOptions"),
];

/// The model's expected Lua option/query fields for one op.
///
/// Positional Lua arguments (collection/slug, `id`, `data`, `documents`,
/// `version_id`) are structural, not option fields; `override_access` is the
/// Lua surface's trust mechanism and exists on every op by construction.
fn expected_lua_fields(w: &OpWire, positional: &[&str]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    out.insert("override_access".to_string());

    for field in w.fields {
        if !field.surfaces.contains(WireSurfaces::LUA) {
            continue;
        }
        if matches!(
            field.kind,
            WireKind::DataFields | WireKind::DataObject | WireKind::DocumentsArray
        ) {
            continue;
        }
        if positional.contains(&field.name) {
            continue;
        }
        out.insert(field.name.to_string());
    }
    out
}

/// Every Lua option/query class carries exactly the model's Lua-surface
/// fields. The classes are generated from the Rust opts structs by
/// `cargo xtask gen-lua-types`, so this effectively checks the CODE's opts
/// structs against the model.
#[test]
fn lua_option_classes_match_wire_model() {
    for (op, classes, positional) in COLLECTION_LUA {
        let w = wire::collection_op(op).expect("wire model covers every op");
        let mut actual = BTreeSet::new();
        for class in *classes {
            actual.extend(lua_class_fields(class));
        }
        assert_same(op, "Lua", &expected_lua_fields(w, positional), &actual);
    }

    for (op, class) in GLOBAL_LUA {
        let w = wire::global_op(op).expect("wire model covers every global op");
        assert_same(
            op,
            "Lua",
            &expected_lua_fields(w, &[]),
            &lua_class_fields(class),
        );
    }
}

/// Every Lua option field's annotation carries the type its `WireKind` names.
/// The classes are generated from the Rust opts structs, so this checks the
/// CODE's types against the model — the name-level check above cannot tell a
/// `limit` typed `string` from one typed `integer`.
#[test]
fn lua_option_class_types_match_wire_model() {
    for (op, classes, _) in COLLECTION_LUA {
        let w = wire::collection_op(op).expect("wire model covers every op");
        assert_lua_types(w, classes);
    }

    for (op, class) in GLOBAL_LUA {
        let w = wire::global_op(op).expect("wire model covers every global op");
        assert_lua_types(w, &[*class]);
    }
}

/// Compare one op's Lua annotations against the model. A field the classes do
/// not declare is the name checker's business, not this one's.
fn assert_lua_types(w: &OpWire, classes: &[&str]) {
    let mut declared = BTreeMap::new();
    for class in classes {
        declared.extend(lua_class_typed_fields(class));
    }

    for field in w.fields {
        if !field.surfaces.contains(WireSurfaces::LUA) {
            continue;
        }

        let (Some(expected), Some(actual)) =
            (expected_lua_ty(field.kind), declared.get(field.name))
        else {
            continue;
        };

        assert_eq!(
            actual, expected,
            "op `{}` field `{}`: the Lua annotation type disagrees with the wire model — \
             fix the opts struct in the code or the kind in src/service/op/wire.rs",
            w.op, field.name
        );
    }
}

// ── Job ops ─────────────────────────────────────────────────────────────
//
// Unlike CRUD, the identifying argument (`slug`/`id`) is a modeled field on
// every surface, so the proto comparison includes tag 1 instead of treating
// it as structural routing. The Lua side needs no checker here: the option
// tables of `crap.jobs.queue` / `crap.jobs.list_runs` reject unknown keys
// against `OpWire::lua_option_keys`, so their accepted keys are *generated*
// from the model rather than diffed against it.

/// Every job request message carries exactly the model's GRPC-surface
/// fields. `list_jobs` has an empty request and no pin — nothing can drift
/// in an empty message.
#[test]
fn job_proto_messages_match_wire_model() {
    let map = [
        ("trigger_job", "TriggerJobRequest"),
        ("cancel_job_run", "CancelJobRunRequest"),
        ("get_job_run", "GetJobRunRequest"),
        ("list_job_runs", "ListJobRunsRequest"),
    ];

    for (op, message) in map {
        let w = wire::job_op(op).expect("wire model covers every job op");

        let expected: BTreeSet<String> = w
            .fields
            .iter()
            .filter(|f| f.surfaces.contains(WireSurfaces::GRPC))
            .map(|f| f.grpc_name().to_string())
            .collect();

        assert_same(op, "proto", &expected, &proto_message_fields(message));
    }
}

/// The pinned proto spec and the model's GRPC surface agree field-for-field
/// on every job op, and the map above names every op the model declares.
#[test]
fn job_proto_spec_covers_exactly_the_grpc_surface() {
    for w in wire::JOB_OPS {
        if w.op == "list_jobs" {
            assert!(
                wire_proto::proto_message(w.op).is_none(),
                "list_jobs takes no arguments; an empty message needs no pin"
            );
            continue;
        }

        let msg = wire_proto::proto_message(w.op)
            .unwrap_or_else(|| panic!("no pinned proto message for job op `{}`", w.op));
        let spec_names: BTreeSet<&str> = msg.fields.iter().map(|f| f.name).collect();

        let model_names: BTreeSet<&str> = w
            .fields
            .iter()
            .filter(|f| f.surfaces.contains(WireSurfaces::GRPC))
            .map(WireField::grpc_name)
            .collect();

        assert_eq!(
            model_names, spec_names,
            "job op `{}`: wire model GRPC surface and pinned proto spec diverge",
            w.op
        );
    }
}
