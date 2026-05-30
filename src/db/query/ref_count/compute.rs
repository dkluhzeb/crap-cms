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
