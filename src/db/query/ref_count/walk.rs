//! Recursive walker that collects outgoing refs from a nested-composite
//! JSON object — an array row or a block's reconstructed `data`.
//!
//! Inside array rows and block data, fields are keyed by their bare name
//! (`field.name`) and composites nest as JSON objects/arrays. This differs
//! from the document top level, where group fields are flattened to
//! `group__field` columns (handled by [`super::compute`] / [`super::read`]).
//! Once the top-level walk descends into an array row or a block, every
//! deeper container — Group, Array, Blocks, Row/Collapsible/Tabs — nests as
//! JSON, so a single recursive walker covers every combination at any depth,
//! counting both has-one and has-many relationships.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::core::{BlockDefinition, FieldDefinition, FieldType};
use crate::db::query::join::{parse_id_list, parse_polymorphic_values};

use super::outgoing_ref::{OutgoingRef, push_ref};

/// Walk one nested object against its field definitions, collecting refs.
pub(super) fn walk_nested_refs(
    obj: &Map<String, Value>,
    fields: &[FieldDefinition],
    refs: &mut Vec<OutgoingRef>,
) {
    for field in fields {
        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                let Some(rc) = &field.relationship else {
                    continue;
                };
                let Some(value) = obj.get(&field.name) else {
                    continue;
                };

                if rc.has_many {
                    push_has_many(value, rc.is_polymorphic(), &rc.collection, refs);
                } else if let Some(s) = value.as_str() {
                    push_ref(refs, s, rc.is_polymorphic(), &rc.collection);
                }
            }

            FieldType::Group => {
                if let Some(Value::Object(inner)) = obj.get(&field.name) {
                    walk_nested_refs(inner, &field.fields, refs);
                }
            }

            FieldType::Array => {
                if let Some(Value::Array(rows)) = obj.get(&field.name) {
                    for row in rows {
                        if let Value::Object(row_obj) = row {
                            walk_nested_refs(row_obj, &field.fields, refs);
                        }
                    }
                }
            }

            FieldType::Blocks => {
                if let Some(Value::Array(blocks)) = obj.get(&field.name) {
                    walk_block_values(blocks, &field.blocks, refs);
                }
            }

            FieldType::Row | FieldType::Collapsible => {
                walk_nested_refs(obj, &field.fields, refs);
            }

            FieldType::Tabs => {
                for tab in &field.tabs {
                    walk_nested_refs(obj, &tab.fields, refs);
                }
            }

            _ => {}
        }
    }
}

/// Walk a list of block instances (`_block_type` + nested data fields),
/// matching each to its definition and recursing into its fields.
pub(super) fn walk_block_values(
    blocks: &[Value],
    defs: &[BlockDefinition],
    refs: &mut Vec<OutgoingRef>,
) {
    for block in blocks {
        let Value::Object(obj) = block else {
            continue;
        };
        let Some(block_type) = obj.get("_block_type").and_then(Value::as_str) else {
            continue;
        };
        let Some(def) = defs.iter().find(|b| b.block_type == block_type) else {
            continue;
        };

        walk_nested_refs(obj, &def.fields, refs);
    }
}

/// Push refs for a has-many relationship value — an array of ids, or
/// polymorphic `collection/id` entries. Deduplicated within the value to
/// mirror the `SELECT DISTINCT` the top-level junction path uses.
fn push_has_many(
    value: &Value,
    is_polymorphic: bool,
    default_collection: &str,
    refs: &mut Vec<OutgoingRef>,
) {
    if is_polymorphic {
        let mut seen = HashSet::new();

        for (coll, id) in parse_polymorphic_values(value) {
            if seen.insert((coll.clone(), id.clone())) {
                push_ref(refs, &format!("{coll}/{id}"), true, "");
            }
        }
    } else {
        let mut seen = HashSet::new();

        for id in parse_id_list(value) {
            if seen.insert(id.clone()) {
                push_ref(refs, &id, false, default_collection);
            }
        }
    }
}
