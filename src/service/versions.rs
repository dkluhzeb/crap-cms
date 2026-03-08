//! Version snapshot helpers: create, save draft, prune.

use std::collections::HashMap;

use anyhow::Result;

use crate::core::collection::VersionsConfig;
use crate::core::document::Document;
use crate::core::field::FieldDefinition;
use crate::db::query;

/// Recursively merge join-table data (blocks, arrays, relationships) into a snapshot,
/// handling Tabs/Row/Collapsible layout wrappers.
fn merge_join_data_into_snapshot(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    fields: &[FieldDefinition],
    data: &HashMap<String, serde_json::Value>,
) {
    use crate::core::field::FieldType;
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

/// Save a draft-only version: merge incoming hook-processed data onto existing doc,
/// create a version snapshot, and prune.
pub(crate) fn save_draft_version(
    conn: &rusqlite::Connection,
    table: &str,
    parent_id: &str,
    fields: &[FieldDefinition],
    versions: Option<&VersionsConfig>,
    existing_doc: &Document,
    final_ctx_data: &HashMap<String, serde_json::Value>,
) -> Result<()> {
    let mut snapshot_fields = existing_doc.fields.clone();
    for (k, v) in final_ctx_data {
        snapshot_fields.insert(k.clone(), v.clone());
    }
    let snapshot_doc = Document {
        id: parent_id.to_string(),
        fields: snapshot_fields,
        created_at: existing_doc.created_at.clone(),
        updated_at: existing_doc.updated_at.clone(),
    };

    let mut snapshot = query::build_snapshot(conn, table, fields, &snapshot_doc)?;
    if let Some(obj) = snapshot.as_object_mut() {
        merge_join_data_into_snapshot(obj, fields, final_ctx_data);
    }
    query::create_version(conn, table, parent_id, "draft", &snapshot)?;
    prune_versions(conn, table, parent_id, versions)?;
    Ok(())
}

/// Set document status, create a version snapshot, and prune.
pub(crate) fn create_version_snapshot(
    conn: &rusqlite::Connection,
    table: &str,
    parent_id: &str,
    fields: &[FieldDefinition],
    versions: Option<&VersionsConfig>,
    has_drafts: bool,
    status: &str,
    doc: &Document,
) -> Result<()> {
    if has_drafts {
        query::set_document_status(conn, table, parent_id, status)?;
    }
    let snapshot = query::build_snapshot(conn, table, fields, doc)?;
    query::create_version(conn, table, parent_id, status, &snapshot)?;
    prune_versions(conn, table, parent_id, versions)?;
    Ok(())
}

/// Prune versions if max_versions is configured and > 0.
pub(crate) fn prune_versions(
    conn: &rusqlite::Connection,
    table: &str,
    parent_id: &str,
    versions: Option<&VersionsConfig>,
) -> Result<()> {
    if let Some(vc) = versions {
        if vc.max_versions > 0 {
            query::prune_versions(conn, table, parent_id, vc.max_versions)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use rusqlite::Connection;
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::query;
    use crate::service::{persist_create, persist_update, persist_draft_version, persist_unpublish};

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![
            FieldDefinition {
                name: "title".to_string(),
                ..Default::default()
            },
        ];
        def
    }

    fn versioned_def() -> CollectionDefinition {
        let mut def = test_def();
        def.versions = Some(VersionsConfig::new(true, 10));
        def
    }

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
                _version INTEGER NOT NULL,
                _status TEXT NOT NULL DEFAULT 'published',
                _latest INTEGER NOT NULL DEFAULT 0,
                snapshot TEXT NOT NULL,
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            )"
        ).unwrap();
        conn
    }

    #[test]
    fn persist_create_with_versions() {
        let conn = setup_db();
        let def = versioned_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Versioned".to_string());

        let doc = persist_create(&conn, "posts", &def, &data, &HashMap::new(), None, None, false).unwrap();
        assert_eq!(doc.get_str("title"), Some("Versioned"));

        let count = query::count_versions(&conn, "posts", &doc.id).unwrap();
        assert_eq!(count, 1, "should have 1 version after create");
    }

    #[test]
    fn persist_create_draft() {
        let conn = setup_db();
        let def = versioned_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Draft Post".to_string());

        let doc = persist_create(&conn, "posts", &def, &data, &HashMap::new(), None, None, true).unwrap();
        assert_eq!(doc.get_str("title"), Some("Draft Post"));

        let status = query::get_document_status(&conn, "posts", &doc.id).unwrap();
        assert_eq!(status.as_deref(), Some("draft"));
    }

    #[test]
    fn persist_update_with_versions() {
        let conn = setup_db();
        let def = versioned_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "V1".to_string());

        let doc = persist_create(&conn, "posts", &def, &data, &HashMap::new(), None, None, false).unwrap();
        let id = doc.id.clone();

        let mut update_data = HashMap::new();
        update_data.insert("title".to_string(), "V2".to_string());
        persist_update(&conn, "posts", &id, &def, &update_data, &HashMap::new(), None, None).unwrap();

        let count = query::count_versions(&conn, "posts", &id).unwrap();
        assert_eq!(count, 2, "should have 2 versions after create + update");
    }

    #[test]
    fn persist_draft_version_does_not_modify_main_table() {
        let conn = setup_db();
        let def = versioned_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Published".to_string());

        let doc = persist_create(&conn, "posts", &def, &data, &HashMap::new(), None, None, false).unwrap();
        let id = doc.id.clone();

        let mut draft_data = HashMap::new();
        draft_data.insert("title".to_string(), serde_json::json!("Draft Title"));

        let existing = persist_draft_version(&conn, "posts", &id, &def, &draft_data, None).unwrap();
        assert_eq!(existing.get_str("title"), Some("Published"), "main table should not be modified");

        let count = query::count_versions(&conn, "posts", &id).unwrap();
        assert_eq!(count, 2, "should have 2 versions (create published + draft)");
    }

    #[test]
    fn persist_unpublish_sets_draft_status() {
        let conn = setup_db();
        let def = versioned_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "To Unpublish".to_string());

        let doc = persist_create(&conn, "posts", &def, &data, &HashMap::new(), None, None, false).unwrap();
        let id = doc.id.clone();

        let status = query::get_document_status(&conn, "posts", &id).unwrap();
        assert_eq!(status.as_deref(), Some("published"));

        let result = persist_unpublish(&conn, "posts", &id, &def).unwrap();
        assert_eq!(result.id, id);

        let status = query::get_document_status(&conn, "posts", &id).unwrap();
        assert_eq!(status.as_deref(), Some("draft"));

        let count = query::count_versions(&conn, "posts", &id).unwrap();
        assert_eq!(count, 2, "should have 2 versions (published create + draft unpublish)");
    }

    #[test]
    fn version_pruning() {
        let conn = setup_db();
        let mut def = test_def();
        def.versions = Some(VersionsConfig::new(true, 2));

        let mut data = HashMap::new();
        data.insert("title".to_string(), "V1".to_string());
        let doc = persist_create(&conn, "posts", &def, &data, &HashMap::new(), None, None, false).unwrap();
        let id = doc.id.clone();

        for i in 2..=4 {
            let mut update_data = HashMap::new();
            update_data.insert("title".to_string(), format!("V{}", i));
            persist_update(&conn, "posts", &id, &def, &update_data, &HashMap::new(), None, None).unwrap();
        }

        let count = query::count_versions(&conn, "posts", &id).unwrap();
        assert_eq!(count, 2, "versions should be pruned to max_versions=2");
    }

    #[test]
    fn draft_version_includes_blocks_inside_tabs() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT,
                updated_at TEXT
            );
            CREATE TABLE posts_content (
                id TEXT PRIMARY KEY,
                parent_id TEXT,
                _order INTEGER,
                _block_type TEXT,
                data TEXT
            );
            CREATE TABLE _versions_posts (
                id TEXT PRIMARY KEY,
                _parent TEXT NOT NULL,
                _version INTEGER NOT NULL,
                _status TEXT NOT NULL DEFAULT 'published',
                _latest INTEGER NOT NULL DEFAULT 0,
                snapshot TEXT NOT NULL,
                created_at TEXT DEFAULT (datetime('now')),
                updated_at TEXT DEFAULT (datetime('now'))
            )"
        ).unwrap();

        let blocks_field = FieldDefinition {
            name: "content".to_string(),
            field_type: FieldType::Blocks,
            ..Default::default()
        };
        let tabs_field = FieldDefinition {
            name: "page_settings".to_string(),
            field_type: FieldType::Tabs,
            tabs: vec![FieldTab::new("Content", vec![blocks_field])],
            ..Default::default()
        };
        let mut def = test_def();
        def.versions = Some(VersionsConfig::new(true, 10));
        def.fields = vec![
            FieldDefinition { name: "title".to_string(), ..Default::default() },
            tabs_field,
        ];

        let mut data = HashMap::new();
        data.insert("title".to_string(), "Page 1".to_string());
        let doc = persist_create(&conn, "posts", &def, &data, &HashMap::new(), None, None, false).unwrap();
        let id = doc.id.clone();

        let mut hook_data: HashMap<String, serde_json::Value> = HashMap::new();
        hook_data.insert("title".to_string(), serde_json::json!("Page 1 Draft"));
        hook_data.insert("content".to_string(), serde_json::json!([
            {"_block_type": "hero", "heading": "Welcome"},
            {"_block_type": "text", "body": "Hello world"},
        ]));

        persist_draft_version(&conn, "posts", &id, &def, &hook_data, None).unwrap();

        let versions = query::list_versions(&conn, "posts", &id, None, None).unwrap();
        let draft_version = versions.iter().find(|v| v.status == "draft")
            .expect("should have a draft version");
        let snapshot = draft_version.snapshot.as_object().unwrap();
        let content = snapshot.get("content")
            .expect("draft snapshot must include blocks from inside Tabs");
        let blocks = content.as_array().unwrap();
        assert_eq!(blocks.len(), 2, "draft snapshot should have 2 blocks");
        assert_eq!(blocks[0]["_block_type"], "hero");
        assert_eq!(blocks[1]["_block_type"], "text");
    }
}
