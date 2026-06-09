//! Recursive walker that visits outgoing refs from a nested-composite JSON
//! object — an array row or a block's reconstructed `data`.
//!
//! Inside array rows and block data, fields are keyed by their bare name
//! (`field.name`) and composites nest as JSON objects/arrays. This differs
//! from the document top level, where group fields are flattened to
//! `group__field` columns (handled by [`super::compute`] / [`super::read`]).
//! Once the top-level walk descends into an array row or a block, every
//! deeper container — Group, Array, Blocks, Row/Collapsible/Tabs — nests as
//! JSON, so a single recursive walker covers every combination at any depth,
//! counting both has-one and has-many relationships.
//!
//! [`walk_nested_with`] is the single source of truth for this traversal.
//! Ref-counting wraps it to collect [`OutgoingRef`]s (ignoring the path);
//! the back-reference scanner wraps it to record the field-name path to each
//! matched reference (for precise `field_name`/label reporting). No other
//! code should re-encode the container-recursion rules.

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::core::{BlockDefinition, FieldDefinition, FieldType, RelationshipConfig};
use crate::db::query::join::{parse_id_list, parse_polymorphic_values};

use super::outgoing_ref::{OutgoingRef, push_ref};

/// One segment of the path from a nested walk's root to a matched reference.
///
/// Transparent layout wrappers (Row/Collapsible/Tabs) contribute no segment;
/// Group/Array push their own [`FieldDefinition`]; Blocks push their field
/// then the matched [`BlockDefinition`]. The leaf relationship field is passed
/// to the visitor separately, so it is not on the stack.
pub(crate) enum RefPathSeg<'a> {
    Field(&'a FieldDefinition),
    Block(&'a BlockDefinition),
}

/// Generic nested-composite walker — the single traversal primitive.
///
/// Calls `visit(leaf_field, ancestor_path, target_collection, target_id,
/// is_polymorphic)` once per resolved relationship/upload reference, at any
/// nesting depth. Has-many values are deduplicated within a single field value
/// (mirroring the junction `SELECT DISTINCT`); malformed/empty refs are
/// dropped.
pub(crate) fn walk_nested_with<'a, V>(
    obj: &Map<String, Value>,
    fields: &'a [FieldDefinition],
    stack: &mut Vec<RefPathSeg<'a>>,
    visit: &mut V,
) where
    V: FnMut(&'a FieldDefinition, &[RefPathSeg<'a>], &str, &str, bool),
{
    for field in fields {
        match field.field_type {
            FieldType::Relationship | FieldType::Upload => {
                let Some(rc) = &field.relationship else {
                    continue;
                };
                let Some(value) = obj.get(&field.name) else {
                    continue;
                };

                emit_rel(field, rc, value, stack, visit);
            }

            FieldType::Group => {
                if let Some(Value::Object(inner)) = obj.get(&field.name) {
                    stack.push(RefPathSeg::Field(field));
                    walk_nested_with(inner, &field.fields, stack, visit);
                    stack.pop();
                }
            }

            FieldType::Array => {
                if let Some(Value::Array(rows)) = obj.get(&field.name) {
                    stack.push(RefPathSeg::Field(field));
                    for row in rows {
                        if let Value::Object(row_obj) = row {
                            walk_nested_with(row_obj, &field.fields, stack, visit);
                        }
                    }
                    stack.pop();
                }
            }

            FieldType::Blocks => {
                if let Some(Value::Array(blocks)) = obj.get(&field.name) {
                    stack.push(RefPathSeg::Field(field));
                    walk_blocks_with(blocks, &field.blocks, stack, visit);
                    stack.pop();
                }
            }

            FieldType::Row | FieldType::Collapsible => {
                walk_nested_with(obj, &field.fields, stack, visit);
            }

            FieldType::Tabs => {
                for tab in &field.tabs {
                    walk_nested_with(obj, &tab.fields, stack, visit);
                }
            }

            _ => {}
        }
    }
}

/// Walk a list of block instances (`_block_type` + nested data fields),
/// matching each to its definition and recursing into its fields. Pushes a
/// [`RefPathSeg::Block`] for the matched definition so the visitor sees the
/// block type in the path.
pub(crate) fn walk_blocks_with<'a, V>(
    blocks: &[Value],
    defs: &'a [BlockDefinition],
    stack: &mut Vec<RefPathSeg<'a>>,
    visit: &mut V,
) where
    V: FnMut(&'a FieldDefinition, &[RefPathSeg<'a>], &str, &str, bool),
{
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

        stack.push(RefPathSeg::Block(def));
        walk_nested_with(obj, &def.fields, stack, visit);
        stack.pop();
    }
}

/// Resolve a relationship/upload field's value into `(collection, id)` pairs
/// and emit each via the visitor. Deduplicates has-many values within the
/// field, drops empty/malformed entries.
fn emit_rel<'a, V>(
    field: &'a FieldDefinition,
    rc: &RelationshipConfig,
    value: &Value,
    stack: &mut [RefPathSeg<'a>],
    visit: &mut V,
) where
    V: FnMut(&'a FieldDefinition, &[RefPathSeg<'a>], &str, &str, bool),
{
    let collection: &str = &rc.collection;

    if rc.has_many {
        if rc.is_polymorphic() {
            let mut seen = HashSet::new();

            for (coll, id) in parse_polymorphic_values(value) {
                if !coll.is_empty() && !id.is_empty() && seen.insert((coll.clone(), id.clone())) {
                    visit(field, stack, &coll, &id, true);
                }
            }
        } else {
            let mut seen = HashSet::new();

            for id in parse_id_list(value) {
                if !id.is_empty() && seen.insert(id.clone()) {
                    visit(field, stack, collection, &id, false);
                }
            }
        }

        return;
    }

    let Some(s) = value.as_str() else {
        return;
    };

    if rc.is_polymorphic() {
        if let Some((coll, id)) = s.split_once('/')
            && !coll.is_empty()
            && !id.is_empty()
        {
            visit(field, stack, coll, id, true);
        }
    } else if !s.is_empty() {
        visit(field, stack, collection, s, false);
    }
}

/// Walk one nested object against its field definitions, collecting refs.
/// Thin ref-counting wrapper over [`walk_nested_with`] — ignores the path.
pub(super) fn walk_nested_refs(
    obj: &Map<String, Value>,
    fields: &[FieldDefinition],
    refs: &mut Vec<OutgoingRef>,
) {
    let mut stack = Vec::new();
    walk_nested_with(
        obj,
        fields,
        &mut stack,
        &mut |_field, _path, coll, id, poly| {
            push_resolved(refs, coll, id, poly);
        },
    );
}

/// Walk a list of block instances, collecting refs. Thin ref-counting wrapper
/// over [`walk_blocks_with`].
pub(super) fn walk_block_values(
    blocks: &[Value],
    defs: &[BlockDefinition],
    refs: &mut Vec<OutgoingRef>,
) {
    let mut stack = Vec::new();
    walk_blocks_with(
        blocks,
        defs,
        &mut stack,
        &mut |_field, _path, coll, id, poly| {
            push_resolved(refs, coll, id, poly);
        },
    );
}

/// Push an already-resolved `(collection, id)` ref through [`push_ref`] so the
/// `OutgoingRef` construction stays in one place.
fn push_resolved(refs: &mut Vec<OutgoingRef>, collection: &str, id: &str, is_polymorphic: bool) {
    if is_polymorphic {
        push_ref(refs, &format!("{collection}/{id}"), true, "");
    } else {
        push_ref(refs, id, false, collection);
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

    #[test]
    fn array_in_array_relationship_is_collected() {
        // array → array → rel: the inner array is a JSON array nested in the
        // outer row object. The single walker descends it at any depth.
        let fields = vec![
            FieldDefinition::builder("outer", FieldType::Array)
                .fields(vec![
                    FieldDefinition::builder("inner", FieldType::Array)
                        .fields(vec![rel("tag", false)])
                        .build(),
                ])
                .build(),
        ];
        let data = json!({
            "outer": [
                { "inner": [{ "tag": "t1" }, { "tag": "t2" }] },
                { "inner": [{ "tag": "t3" }] }
            ]
        });
        assert_eq!(
            collect(&data, &fields),
            vec![
                ("tags".into(), "t1".into()),
                ("tags".into(), "t2".into()),
                ("tags".into(), "t3".into())
            ]
        );
    }

    #[test]
    fn path_records_group_and_leaf_for_back_references() {
        // The path-aware visitor sees the ancestor chain (Group) plus the leaf
        // field — this is what the back-reference scanner uses for labels.
        let fields = vec![
            FieldDefinition::builder("g", FieldType::Group)
                .fields(vec![rel("tag", false)])
                .build(),
        ];
        let obj = json!({ "g": { "tag": "t1" } });

        let mut seen: Vec<(Vec<String>, String)> = Vec::new();
        let mut stack = Vec::new();
        walk_nested_with(
            obj.as_object().unwrap(),
            &fields,
            &mut stack,
            &mut |leaf, path, _coll, id, _poly| {
                let names: Vec<String> = path
                    .iter()
                    .map(|seg| match seg {
                        RefPathSeg::Field(f) => f.name.clone(),
                        RefPathSeg::Block(b) => b.block_type.clone(),
                    })
                    .chain(std::iter::once(leaf.name.clone()))
                    .collect();
                seen.push((names, id.to_string()));
            },
        );

        assert_eq!(
            seen,
            vec![(vec!["g".to_string(), "tag".to_string()], "t1".to_string())]
        );
    }
}
