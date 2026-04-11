//! Draft version save: merge data onto existing doc, snapshot, prune.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    core::{Document, FieldDefinition, FieldType, collection::VersionsConfig},
    db::{DbConnection, query},
};

use super::snapshot::prune_versions;

/// Save a draft-only version: merge incoming hook-processed data onto existing doc,
/// create a version snapshot, and prune.
pub(crate) fn save_draft_version(
    conn: &dyn DbConnection,
    table: &str,
    parent_id: &str,
    fields: &[FieldDefinition],
    versions: Option<&VersionsConfig>,
    existing_doc: &Document,
    final_ctx_data: &HashMap<String, Value>,
) -> Result<()> {
    let mut snapshot_fields = existing_doc.fields.clone();

    for (k, v) in final_ctx_data {
        snapshot_fields.insert(k.clone(), v.clone());
    }

    let snapshot_doc = Document::builder(parent_id)
        .fields(snapshot_fields)
        .created_at(existing_doc.created_at.as_deref())
        .updated_at(existing_doc.updated_at.as_deref())
        .build();

    let mut snapshot = query::build_snapshot(conn, table, fields, &snapshot_doc)?;

    if let Some(obj) = snapshot.as_object_mut() {
        merge_join_data_into_snapshot(obj, fields, final_ctx_data);
    }

    query::create_version(conn, table, parent_id, "draft", &snapshot)?;

    prune_versions(conn, table, parent_id, versions)?;

    Ok(())
}

/// Recursively merge join-table data (blocks, arrays, relationships) into a snapshot,
/// handling Tabs/Row/Collapsible layout wrappers.
fn merge_join_data_into_snapshot(
    obj: &mut Map<String, Value>,
    fields: &[FieldDefinition],
    data: &HashMap<String, Value>,
) {
    for field in fields {
        match field.field_type {
            FieldType::Array | FieldType::Blocks | FieldType::Relationship => {
                if let Some(v) = data.get(&field.name) {
                    obj.insert(field.name.clone(), v.clone());
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                merge_join_data_into_snapshot(obj, &field.fields, data);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    merge_join_data_into_snapshot(obj, &tab.fields, data);
                }
            }
            _ => {}
        }
    }
}
