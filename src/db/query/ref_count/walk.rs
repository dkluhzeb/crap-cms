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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::core::field::{BlockDefinition, FieldDefinition, FieldType, RelationshipConfig};

    use super::*;

    /// A relationship sub-field pointing at the `tags` collection.
    fn rel(name: &str, has_many: bool) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", has_many))
            .build()
    }

    /// Walk `obj` and return the collected refs as sorted `(collection, id)`
    /// pairs so assertions are order-independent.
    fn collect(obj: &Value, fields: &[FieldDefinition]) -> Vec<(String, String)> {
        let mut refs = Vec::new();
        walk_nested_refs(obj.as_object().unwrap(), fields, &mut refs);
        let mut pairs: Vec<(String, String)> = refs
            .into_iter()
            .map(|r| (r.target_collection, r.target_id))
            .collect();
        pairs.sort();
        pairs
    }

    #[test]
    fn has_one_relationship_is_collected() {
        let fields = vec![rel("tag", false)];
        assert_eq!(
            collect(&json!({ "tag": "t1" }), &fields),
            vec![("tags".into(), "t1".into())]
        );
    }

    #[test]
    fn has_many_relationship_collects_all_and_dedups() {
        let fields = vec![rel("tags", true)];
        // duplicates collapse — `_ref_count` is an edge set, not a multiset.
        assert_eq!(
            collect(&json!({ "tags": ["t1", "t2", "t1"] }), &fields),
            vec![("tags".into(), "t1".into()), ("tags".into(), "t2".into())]
        );
    }

    #[test]
    fn recurses_into_nested_group() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![rel("tag", false)])
                .build(),
        ];
        assert_eq!(
            collect(&json!({ "meta": { "tag": "t1" } }), &fields),
            vec![("tags".into(), "t1".into())]
        );
    }

    #[test]
    fn recurses_into_array_rows() {
        let fields = vec![
            FieldDefinition::builder("rows", FieldType::Array)
                .fields(vec![rel("tag", false)])
                .build(),
        ];
        assert_eq!(
            collect(
                &json!({ "rows": [{ "tag": "t1" }, { "tag": "t2" }] }),
                &fields
            ),
            vec![("tags".into(), "t1".into()), ("tags".into(), "t2".into())]
        );
    }

    #[test]
    fn recurses_into_matching_block_only() {
        let fields = vec![
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new("card", vec![rel("tag", false)])])
                .build(),
        ];
        // Only the block whose `_block_type` matches a definition contributes;
        // an unknown block type is skipped.
        let data = json!({
            "content": [
                { "_block_type": "card", "tag": "t1" },
                { "_block_type": "mystery", "tag": "t2" }
            ]
        });
        assert_eq!(collect(&data, &fields), vec![("tags".into(), "t1".into())]);
    }

    #[test]
    fn layout_wrappers_are_transparent() {
        // Row/Collapsible add no nesting — their children read from the same
        // object, so `tag` sits at the top level, not under "layout".
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![rel("tag", false)])
                .build(),
        ];
        assert_eq!(
            collect(&json!({ "tag": "t1" }), &fields),
            vec![("tags".into(), "t1".into())]
        );
    }

    #[test]
    fn deep_combination_array_group_and_block_group() {
        // array → group → rel, plus blocks → group → rel, in one object.
        let fields = vec![
            FieldDefinition::builder("rows", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("g", FieldType::Group)
                        .fields(vec![rel("tag", false)])
                        .build(),
                ])
                .build(),
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new(
                    "card",
                    vec![
                        FieldDefinition::builder("meta", FieldType::Group)
                            .fields(vec![rel("tag", false)])
                            .build(),
                    ],
                )])
                .build(),
        ];
        let data = json!({
            "rows": [{ "g": { "tag": "a" } }],
            "content": [{ "_block_type": "card", "meta": { "tag": "b" } }]
        });
        assert_eq!(
            collect(&data, &fields),
            vec![("tags".into(), "a".into()), ("tags".into(), "b".into())]
        );
    }
}
