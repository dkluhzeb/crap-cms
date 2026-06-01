//! Draft version save: merge data onto existing doc, snapshot, prune.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    core::{Document, DocumentFields, FieldDefinition, FieldType, collection::VersionsConfig},
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
    final_ctx_data: &DocumentFields,
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
    data: &DocumentFields,
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::core::field::RelationshipConfig;

    use super::*;

    #[test]
    fn copies_join_field_values_recurses_layout_ignores_scalars_and_groups() {
        let fields = vec![
            FieldDefinition::builder("tags", FieldType::Relationship)
                .relationship(RelationshipConfig::new("tags", true))
                .build(),
            FieldDefinition::builder("items", FieldType::Array).build(),
            FieldDefinition::builder("title", FieldType::Text).build(), // scalar → ignored
            FieldDefinition::builder("row", FieldType::Row)
                .fields(vec![
                    FieldDefinition::builder("rows", FieldType::Blocks).build(),
                ])
                .build(),
            // Group is NOT a transparent layout here — its join children are
            // not merged by this function.
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("ignored", FieldType::Array).build(),
                ])
                .build(),
        ];

        let mut data = DocumentFields::new();
        data.insert("tags".into(), json!(["t1"]));
        data.insert("items".into(), json!([{ "x": 1 }]));
        data.insert("title".into(), json!("Hello"));
        data.insert("rows".into(), json!([{ "_block_type": "hero" }]));
        data.insert("ignored".into(), json!([{ "y": 2 }]));

        let mut obj = serde_json::Map::new();
        merge_join_data_into_snapshot(&mut obj, &fields, &data);

        assert_eq!(obj.get("tags"), Some(&json!(["t1"])));
        assert_eq!(obj.get("items"), Some(&json!([{ "x": 1 }])));
        assert_eq!(obj.get("rows"), Some(&json!([{ "_block_type": "hero" }]))); // via Row
        assert!(!obj.contains_key("title")); // scalar
        assert!(!obj.contains_key("ignored")); // inside Group → not merged
    }

    #[test]
    fn absent_data_keys_are_skipped() {
        let fields = vec![FieldDefinition::builder("items", FieldType::Array).build()];
        let mut obj = serde_json::Map::new();
        merge_join_data_into_snapshot(&mut obj, &fields, &DocumentFields::new());
        assert!(obj.is_empty());
    }
}
