//! Compute outgoing refs from in-memory write data (no DB I/O).
//!
//! Mirrors [`super::read::read_outgoing_refs`] but reads from the
//! `DocumentFields` map being written instead of querying the database.
//! Used by the create path to skip 5+ round-trips of just-written data.

use std::collections::HashSet;

use serde_json::Value;

use crate::config::LocaleConfig;
use crate::core::{BlockDefinition, DocumentFields, FieldDefinition, FieldType};
use crate::db::query::helpers::{locale_column, prefixed_name};
use crate::db::query::join::{parse_id_list, parse_polymorphic_values};

use super::outgoing_ref::{OutgoingRef, push_ref};
use super::walk::{walk_block_values, walk_nested_refs};

/// Walk the field tree and compute outgoing refs from write data.
///
/// This mirrors `collect_refs` but reads from in-memory data maps instead
/// of querying the database. Used by `after_create_from_data` to eliminate
/// redundant SELECTs of just-written data.
pub(super) fn compute_refs_from_data(
    fields: &[FieldDefinition],
    data: &DocumentFields,
    locale_config: &LocaleConfig,
    prefix: &str,
    refs: &mut Vec<OutgoingRef>,
) {
    for field in fields {
        match field.field_type {
            FieldType::Group => {
                let new_prefix = prefixed_name(prefix, &field.name);
                compute_refs_from_data(&field.fields, data, locale_config, &new_prefix, refs);
            }

            FieldType::Row | FieldType::Collapsible => {
                compute_refs_from_data(&field.fields, data, locale_config, prefix, refs);
            }

            FieldType::Tabs => {
                for tab in &field.tabs {
                    compute_refs_from_data(&tab.fields, data, locale_config, prefix, refs);
                }
            }

            FieldType::Relationship | FieldType::Upload => {
                let Some(rc) = &field.relationship else {
                    continue;
                };
                let col = prefixed_name(prefix, &field.name);

                if field.has_parent_column() {
                    // Has-one: read scalar value from the unified data map.
                    let columns = if field.localized && locale_config.is_enabled() {
                        locale_config
                            .locales
                            .iter()
                            .filter_map(|l| locale_column(&col, l).ok())
                            .collect::<Vec<_>>()
                    } else {
                        vec![col]
                    };

                    for col_name in &columns {
                        if let Some(value) = data.get(col_name).and_then(Value::as_str) {
                            push_ref(refs, value, rc.is_polymorphic(), &rc.collection);
                        }
                    }
                } else {
                    // Has-many: read structured value from the unified data map.
                    // Deduplicate to match the DB path's SELECT DISTINCT —
                    // junction tables can have duplicate rows, and parse_id_list
                    // preserves them. Without dedup, duplicates inflate _ref_count.
                    if let Some(val) = data.get(&col) {
                        if rc.is_polymorphic() {
                            let mut seen = HashSet::new();

                            for (coll, id) in parse_polymorphic_values(val) {
                                if seen.insert((coll.clone(), id.clone())) {
                                    push_ref(refs, &format!("{coll}/{id}"), true, "");
                                }
                            }
                        } else {
                            let mut seen = HashSet::new();

                            for id in parse_id_list(val) {
                                if seen.insert(id.clone()) {
                                    push_ref(refs, &id, false, &rc.collection);
                                }
                            }
                        }
                    }
                }
            }

            FieldType::Array => {
                let col = prefixed_name(prefix, &field.name);
                if let Some(Value::Array(rows)) = data.get(&col) {
                    compute_array_refs_from_data(rows, &field.fields, refs);
                }
            }

            FieldType::Blocks => {
                let col = prefixed_name(prefix, &field.name);
                if let Some(Value::Array(rows)) = data.get(&col) {
                    compute_blocks_refs_from_data(rows, &field.blocks, refs);
                }
            }

            _ => {}
        }
    }
}

/// Extract refs from array row data, recursing into nested composites.
///
/// Each row is a nested-composite JSON object; the shared walker handles
/// relationships at any depth (Group/Array/Blocks inside the row).
fn compute_array_refs_from_data(
    rows: &[Value],
    fields: &[FieldDefinition],
    refs: &mut Vec<OutgoingRef>,
) {
    for row in rows {
        if let Value::Object(obj) = row {
            walk_nested_refs(obj, fields, refs);
        }
    }
}

/// Extract refs from blocks row data, recursing into nested composites.
fn compute_blocks_refs_from_data(
    rows: &[Value],
    blocks: &[BlockDefinition],
    refs: &mut Vec<OutgoingRef>,
) {
    walk_block_values(rows, blocks, refs);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::core::field::{BlockDefinition, FieldDefinition, FieldType, RelationshipConfig};

    use super::*;

    /// A relationship field pointing at the `tags` collection.
    fn rel(name: &str, has_many: bool) -> FieldDefinition {
        FieldDefinition::builder(name, FieldType::Relationship)
            .relationship(RelationshipConfig::new("tags", has_many))
            .build()
    }

    /// A polymorphic relationship (`has_many` controls cardinality).
    fn poly(name: &str, has_many: bool) -> FieldDefinition {
        let mut rc = RelationshipConfig::new("posts", has_many);
        rc.polymorphic = vec!["posts".into(), "pages".into()];
        FieldDefinition::builder(name, FieldType::Relationship)
            .relationship(rc)
            .build()
    }

    /// Run the walker over a `{col: value}` data map and return sorted
    /// `(collection, id)` pairs so assertions are order-independent.
    fn collect(data: serde_json::Value, fields: &[FieldDefinition]) -> Vec<(String, String)> {
        let map: std::collections::HashMap<String, Value> = serde_json::from_value(data).unwrap();
        let data = DocumentFields::from(map);

        let mut refs = Vec::new();
        compute_refs_from_data(fields, &data, &LocaleConfig::default(), "", &mut refs);

        let mut pairs: Vec<(String, String)> = refs
            .into_iter()
            .map(|r| (r.target_collection, r.target_id))
            .collect();
        pairs.sort();
        pairs
    }

    #[test]
    fn has_one_relationship_reads_scalar_column() {
        assert_eq!(
            collect(json!({ "tag": "t1" }), &[rel("tag", false)]),
            vec![("tags".into(), "t1".into())]
        );
    }

    #[test]
    fn has_many_relationship_dedups_to_an_edge_set() {
        // Junction tables can carry duplicate rows; the ref set must not.
        assert_eq!(
            collect(json!({ "tags": ["t1", "t2", "t1"] }), &[rel("tags", true)]),
            vec![("tags".into(), "t1".into()), ("tags".into(), "t2".into())]
        );
    }

    #[test]
    fn group_field_reads_double_underscore_prefixed_column() {
        // A relationship inside group `meta` lives at column `meta__tag`.
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![rel("tag", false)])
                .build(),
        ];
        assert_eq!(
            collect(json!({ "meta__tag": "t1" }), &fields),
            vec![("tags".into(), "t1".into())]
        );
    }

    #[test]
    fn layout_wrappers_are_transparent_no_prefix() {
        // Row adds no column prefix — its child reads the top-level `tag`.
        let fields = vec![
            FieldDefinition::builder("layout", FieldType::Row)
                .fields(vec![rel("tag", false)])
                .build(),
        ];
        assert_eq!(
            collect(json!({ "tag": "t1" }), &fields),
            vec![("tags".into(), "t1".into())]
        );
    }

    #[test]
    fn polymorphic_has_one_parses_collection_slash_id() {
        assert_eq!(
            collect(json!({ "ref": "pages/pg1" }), &[poly("ref", false)]),
            vec![("pages".into(), "pg1".into())]
        );
    }

    #[test]
    fn polymorphic_has_many_dedups_across_collections() {
        assert_eq!(
            collect(
                json!({ "refs": ["posts/p1", "pages/pg1", "posts/p1"] }),
                &[poly("refs", true)]
            ),
            vec![
                ("pages".into(), "pg1".into()),
                ("posts".into(), "p1".into())
            ]
        );
    }

    #[test]
    fn array_rows_recurse_into_nested_relationships() {
        let fields = vec![
            FieldDefinition::builder("rows", FieldType::Array)
                .fields(vec![rel("tag", false)])
                .build(),
        ];
        assert_eq!(
            collect(
                json!({ "rows": [{ "tag": "t1" }, { "tag": "t2" }] }),
                &fields
            ),
            vec![("tags".into(), "t1".into()), ("tags".into(), "t2".into())]
        );
    }

    #[test]
    fn blocks_collect_only_from_matching_block_type() {
        let fields = vec![
            FieldDefinition::builder("content", FieldType::Blocks)
                .blocks(vec![BlockDefinition::new("card", vec![rel("tag", false)])])
                .build(),
        ];
        let data = json!({
            "content": [
                { "_block_type": "card", "tag": "t1" },
                { "_block_type": "mystery", "tag": "t2" }
            ]
        });
        assert_eq!(collect(data, &fields), vec![("tags".into(), "t1".into())]);
    }

    #[test]
    fn missing_and_null_columns_contribute_nothing() {
        assert!(collect(json!({}), &[rel("tag", false)]).is_empty());
        assert!(collect(json!({ "tag": null }), &[rel("tag", false)]).is_empty());
    }
}
