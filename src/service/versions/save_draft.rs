//! Draft version save: merge data onto existing doc, snapshot, prune.

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    core::{
        Document, DocumentFields, FieldDefinition, FieldType, collection::VersionsConfig,
        flatten_group_fields,
    },
    db::{
        DbConnection, LocaleContext, query,
        query::{helpers::prefixed_name, locale_locked_field_names},
    },
};

use super::snapshot::prune_versions;

/// Inputs for [`save_draft_version`]. All fields are required; constructed at
/// each draft-save site (collection persist, global persist, test).
pub(crate) struct SaveDraftArgs<'a> {
    pub conn: &'a dyn DbConnection,
    pub table: &'a str,
    pub parent_id: &'a str,
    pub fields: &'a [FieldDefinition],
    pub versions: Option<&'a VersionsConfig>,
    pub existing_doc: &'a Document,
    pub data: &'a DocumentFields,
    pub locale_ctx: Option<&'a LocaleContext>,
}

/// Save a draft-only version: merge incoming hook-processed data onto existing doc,
/// create a version snapshot, and prune.
pub(crate) fn save_draft_version(args: &SaveDraftArgs<'_>) -> Result<()> {
    let SaveDraftArgs {
        conn,
        table,
        parent_id,
        fields,
        versions,
        existing_doc,
        data: final_ctx_data,
        locale_ctx,
    } = *args;

    let mut snapshot_fields = existing_doc.fields.clone();

    // Locale-locked shared fields must not enter the snapshot from a non-default
    // locale edit — otherwise the canonical default-locale value would be
    // overwritten on restore. Mirrors the published UPDATE path's locale-lock.
    let locked = locale_locked_field_names(fields, locale_ctx);

    // The existing document carries flat group columns (`seo__title`), but the
    // incoming hook data can arrive with nested group objects (`seo: { title }`)
    // from the gRPC/MCP/admin surfaces. Flatten before overlaying so an edited
    // sub-field overwrites the matching flat key instead of leaving a duplicate
    // nested object behind — `build_snapshot`'s hydration would otherwise rebuild
    // the group from the stale flat column and silently drop the edit.
    let mut flattened = flatten_group_fields(final_ctx_data, fields);

    // Drop locale-locked fields (scalar columns AND join fields) so neither the
    // scalar overlay below nor the join re-merge can bake a non-default-locale
    // edit of a shared field into the snapshot.
    flattened.retain(|k, _| !locked.contains(k));

    for (k, v) in &flattened {
        snapshot_fields.insert(k.clone(), v.clone());
    }

    let snapshot_doc = Document::builder(parent_id)
        .fields(snapshot_fields)
        .created_at(existing_doc.created_at.as_deref())
        .updated_at(existing_doc.updated_at.as_deref())
        .build();

    let mut snapshot = query::build_snapshot(conn, table, fields, &snapshot_doc)?;

    // `build_snapshot` rebuilds join data (arrays/blocks/has-many) from the DB —
    // the pre-edit state — so re-overlay the edited join values from the
    // flattened incoming data. `flattened` keys a group-nested join child as
    // `group__child`, which the merge resolves into the nested group object.
    if let Some(obj) = snapshot.as_object_mut() {
        merge_join_data_into_snapshot(obj, fields, &flattened);
    }

    query::create_version(conn, table, parent_id, "draft", &snapshot)?;

    prune_versions(conn, table, parent_id, versions)?;

    Ok(())
}

/// Overlay join-table data (arrays, blocks, has-many relationships) from the
/// flattened incoming edit onto the snapshot. Handles join fields at the top
/// level, nested inside a Group (written into the snapshot's nested group
/// object), and inside transparent Tabs/Row/Collapsible wrappers. The flattened
/// data keys a group child as `group__child`.
fn merge_join_data_into_snapshot(
    obj: &mut Map<String, Value>,
    fields: &[FieldDefinition],
    data: &DocumentFields,
) {
    merge_join_data_prefixed(obj, fields, data, "");
}

/// Inner recursion carrying the group prefix used to look the join value up in
/// the flattened `data` (`""` at the top level, `group` / `group__sub` deeper).
fn merge_join_data_prefixed(
    obj: &mut Map<String, Value>,
    fields: &[FieldDefinition],
    data: &DocumentFields,
    prefix: &str,
) {
    for field in fields {
        match field.field_type {
            FieldType::Array | FieldType::Blocks | FieldType::Relationship => {
                let key = prefixed_name(prefix, &field.name);

                if let Some(v) = data.get(&key) {
                    obj.insert(field.name.clone(), v.clone());
                }
            }
            FieldType::Group => {
                let group_prefix = prefixed_name(prefix, &field.name);

                // The snapshot stores the group as a nested object; merge the
                // group's join children into it (or build it if absent, but only
                // when the edit actually carries group-nested join data).
                if let Some(Value::Object(group_obj)) = obj.get_mut(&field.name) {
                    merge_join_data_prefixed(group_obj, &field.fields, data, &group_prefix);
                } else {
                    let mut group_obj = Map::new();
                    merge_join_data_prefixed(&mut group_obj, &field.fields, data, &group_prefix);

                    if !group_obj.is_empty() {
                        obj.insert(field.name.clone(), Value::Object(group_obj));
                    }
                }
            }
            FieldType::Row | FieldType::Collapsible => {
                merge_join_data_prefixed(obj, &field.fields, data, prefix);
            }
            FieldType::Tabs => {
                for tab in &field.tabs {
                    merge_join_data_prefixed(obj, &tab.fields, data, prefix);
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
            // A group's join child is merged only via its flattened
            // `group__child` key — a bare top-level key of the same name is not
            // picked up (see `merges_group_nested_join_field_into_nested_object`
            // for the positive case).
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
        assert!(!obj.contains_key("ignored")); // group child needs the `meta__ignored` key
        assert!(!obj.contains_key("meta")); // no group-nested join data supplied
    }

    #[test]
    fn absent_data_keys_are_skipped() {
        let fields = vec![FieldDefinition::builder("items", FieldType::Array).build()];
        let mut obj = serde_json::Map::new();
        merge_join_data_into_snapshot(&mut obj, &fields, &DocumentFields::new());
        assert!(obj.is_empty());
    }

    /// Regression: a join field (array/blocks/has-many) nested inside a Group is
    /// merged into the snapshot's nested group object, keyed by its flattened
    /// `group__child` name. `build_snapshot` rebuilds the group's join data from
    /// the DB (pre-edit), so without this the draft edit was silently dropped.
    #[test]
    fn merges_group_nested_join_field_into_nested_object() {
        let fields = vec![
            FieldDefinition::builder("meta", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("things", FieldType::Array).build(),
                ])
                .build(),
        ];

        // Incoming edit, flattened (as `save_draft_version` flattens before merge).
        let mut data = DocumentFields::new();
        data.insert("meta__things".into(), json!([{ "k": 1 }]));

        // Snapshot as `build_snapshot` produced it: the group is nested, its
        // join child holding the stale (empty) DB data.
        let mut obj = serde_json::Map::new();
        obj.insert("meta".into(), json!({ "things": [] }));

        merge_join_data_into_snapshot(&mut obj, &fields, &data);

        assert_eq!(
            obj["meta"]["things"],
            json!([{ "k": 1 }]),
            "the edited group-nested join field must win over the stale DB data"
        );
    }

    /// Regression: a draft update that edits a group sub-field via the nested
    /// data shape (`{ seo: { title } }`, as gRPC/MCP/admin forms send) must not
    /// leave the stale flat `seo__title` from the existing document in the
    /// snapshot. The existing doc carries flat group columns (`seo__title`);
    /// overlaying the incoming nested object without flattening first left BOTH
    /// keys in the snapshot, and `extract_snapshot_recursive` reads the flat one
    /// first (via `or_insert`), so the edit was silently lost on restore.
    #[test]
    fn draft_overlay_flattens_nested_group_subfield() {
        use crate::db::InMemoryConn;

        let conn = InMemoryConn::open();
        conn.execute_batch(
            "CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT,
                _version INTEGER,
                _status TEXT,
                _latest INTEGER DEFAULT 0,
                snapshot TEXT
            );",
        )
        .unwrap();

        let fields = vec![
            FieldDefinition::builder("seo", FieldType::Group)
                .fields(vec![
                    FieldDefinition::builder("title", FieldType::Text).build(),
                ])
                .build(),
        ];

        // Existing document as read from the DB: flat group column.
        let mut existing_fields = DocumentFields::new();
        existing_fields.insert("seo__title".into(), json!("old"));
        let existing_doc = Document::builder("p1").fields(existing_fields).build();

        // Incoming draft edit in the nested shape (gRPC/MCP/admin form).
        let mut incoming = DocumentFields::new();
        incoming.insert("seo".into(), json!({ "title": "new" }));

        save_draft_version(&SaveDraftArgs {
            conn: &conn,
            table: "posts",
            parent_id: "p1",
            fields: &fields,
            versions: None,
            existing_doc: &existing_doc,
            data: &incoming,
            locale_ctx: None,
        })
        .unwrap();

        let versions = query::list_versions(&conn, "posts", "p1", false, None, None).unwrap();
        assert_eq!(versions.len(), 1);

        // `build_snapshot` hydrates flat group columns back into nested form,
        // so the canonical snapshot shape for a group is `seo: { title }`. The
        // edit must be the value that survives.
        let snapshot = &versions[0].snapshot;
        assert_eq!(
            snapshot.pointer("/seo/title"),
            Some(&json!("new")),
            "the draft edit must win over the stale existing value"
        );
    }
}
