//! Version snapshot helpers: create, save draft, prune.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::{Map, Value};

use crate::{
    core::{collection::VersionsConfig, document::Document, field::FieldDefinition},
    db::query,
};

/// Context for creating a version snapshot, bundling the table/document metadata.
pub(crate) struct VersionSnapshotCtx<'a> {
    pub table: &'a str,
    pub parent_id: &'a str,
    pub fields: &'a [FieldDefinition],
    pub versions: Option<&'a VersionsConfig>,
    pub has_drafts: bool,
}

impl<'a> VersionSnapshotCtx<'a> {
    /// Create a builder with the required table and parent_id fields.
    pub fn builder(table: &'a str, parent_id: &'a str) -> VersionSnapshotCtxBuilder<'a> {
        VersionSnapshotCtxBuilder::new(table, parent_id)
    }
}

/// Builder for [`VersionSnapshotCtx`]. Created via [`VersionSnapshotCtx::builder`].
pub(crate) struct VersionSnapshotCtxBuilder<'a> {
    table: &'a str,
    parent_id: &'a str,
    fields: &'a [FieldDefinition],
    versions: Option<&'a VersionsConfig>,
    has_drafts: bool,
}

impl<'a> VersionSnapshotCtxBuilder<'a> {
    fn new(table: &'a str, parent_id: &'a str) -> Self {
        Self {
            table,
            parent_id,
            fields: &[],
            versions: None,
            has_drafts: false,
        }
    }

    pub fn fields(mut self, fields: &'a [FieldDefinition]) -> Self {
        self.fields = fields;
        self
    }

    pub fn versions(mut self, versions: Option<&'a VersionsConfig>) -> Self {
        self.versions = versions;
        self
    }

    pub fn has_drafts(mut self, has_drafts: bool) -> Self {
        self.has_drafts = has_drafts;
        self
    }

    pub fn build(self) -> VersionSnapshotCtx<'a> {
        VersionSnapshotCtx {
            table: self.table,
            parent_id: self.parent_id,
            fields: self.fields,
            versions: self.versions,
            has_drafts: self.has_drafts,
        }
    }
}

/// Recursively merge join-table data (blocks, arrays, relationships) into a snapshot,
/// handling Tabs/Row/Collapsible layout wrappers.
fn merge_join_data_into_snapshot(
    obj: &mut Map<String, Value>,
    fields: &[FieldDefinition],
    data: &HashMap<String, Value>,
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

/// Set document status, create a version snapshot, and prune.
pub(crate) fn create_version_snapshot(
    conn: &rusqlite::Connection,
    ctx: &VersionSnapshotCtx<'_>,
    status: &str,
    doc: &Document,
) -> Result<()> {
    if ctx.has_drafts {
        query::set_document_status(conn, ctx.table, ctx.parent_id, status)?;
    }
    let snapshot = query::build_snapshot(conn, ctx.table, ctx.fields, doc)?;
    query::create_version(conn, ctx.table, ctx.parent_id, status, &snapshot)?;
    prune_versions(conn, ctx.table, ctx.parent_id, ctx.versions)?;
    Ok(())
}

/// Set a document's status to "draft", build+save a snapshot, and prune.
/// Used by both collection `persist_unpublish` and the globals unpublish handler.
pub fn unpublish_with_snapshot(
    conn: &rusqlite::Connection,
    table: &str,
    parent_id: &str,
    fields: &[FieldDefinition],
    versions: Option<&VersionsConfig>,
    doc: &Document,
) -> Result<()> {
    query::set_document_status(conn, table, parent_id, "draft")?;
    let snapshot = query::build_snapshot(conn, table, fields, doc)?;
    query::create_version(conn, table, parent_id, "draft", &snapshot)?;
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
    if let Some(vc) = versions
        && vc.max_versions > 0
    {
        query::prune_versions(conn, table, parent_id, vc.max_versions)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::core::collection::*;
    use crate::core::field::*;
    use crate::db::query;
    use crate::service::{
        PersistOptions, persist_create, persist_draft_version, persist_unpublish, persist_update,
    };
    use rusqlite::Connection;
    use serde_json::{Value, json};
    use std::collections::HashMap;

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
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
            )",
        )
        .unwrap();
        conn
    }

    #[test]
    fn persist_create_with_versions() {
        let conn = setup_db();
        let def = versioned_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Versioned".to_string());

        let doc = persist_create(
            &conn,
            "posts",
            &def,
            &data,
            &HashMap::new(),
            &PersistOptions::default(),
        )
        .unwrap();
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

        let opts = PersistOptions::builder().draft(true).build();
        let doc = persist_create(&conn, "posts", &def, &data, &HashMap::new(), &opts).unwrap();
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

        let doc = persist_create(
            &conn,
            "posts",
            &def,
            &data,
            &HashMap::new(),
            &PersistOptions::default(),
        )
        .unwrap();
        let id = doc.id.clone();

        let mut update_data = HashMap::new();
        update_data.insert("title".to_string(), "V2".to_string());
        persist_update(
            &conn,
            "posts",
            &id,
            &def,
            &update_data,
            &HashMap::new(),
            &PersistOptions::default(),
        )
        .unwrap();

        let count = query::count_versions(&conn, "posts", &id).unwrap();
        assert_eq!(count, 2, "should have 2 versions after create + update");
    }

    #[test]
    fn persist_draft_version_does_not_modify_main_table() {
        let conn = setup_db();
        let def = versioned_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Published".to_string());

        let doc = persist_create(
            &conn,
            "posts",
            &def,
            &data,
            &HashMap::new(),
            &PersistOptions::default(),
        )
        .unwrap();
        let id = doc.id.clone();

        let mut draft_data = HashMap::new();
        draft_data.insert("title".to_string(), json!("Draft Title"));

        let existing = persist_draft_version(&conn, "posts", &id, &def, &draft_data, None).unwrap();
        assert_eq!(
            existing.get_str("title"),
            Some("Published"),
            "main table should not be modified"
        );

        let count = query::count_versions(&conn, "posts", &id).unwrap();
        assert_eq!(
            count, 2,
            "should have 2 versions (create published + draft)"
        );
    }

    #[test]
    fn persist_unpublish_sets_draft_status() {
        let conn = setup_db();
        let def = versioned_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "To Unpublish".to_string());

        let doc = persist_create(
            &conn,
            "posts",
            &def,
            &data,
            &HashMap::new(),
            &PersistOptions::default(),
        )
        .unwrap();
        let id = doc.id.clone();

        let status = query::get_document_status(&conn, "posts", &id).unwrap();
        assert_eq!(status.as_deref(), Some("published"));

        let result = persist_unpublish(&conn, "posts", &id, &def).unwrap();
        assert_eq!(result.id, id);

        let status = query::get_document_status(&conn, "posts", &id).unwrap();
        assert_eq!(status.as_deref(), Some("draft"));

        let count = query::count_versions(&conn, "posts", &id).unwrap();
        assert_eq!(
            count, 2,
            "should have 2 versions (published create + draft unpublish)"
        );
    }

    #[test]
    fn version_pruning() {
        let conn = setup_db();
        let mut def = test_def();
        def.versions = Some(VersionsConfig::new(true, 2));

        let mut data = HashMap::new();
        data.insert("title".to_string(), "V1".to_string());
        let doc = persist_create(
            &conn,
            "posts",
            &def,
            &data,
            &HashMap::new(),
            &PersistOptions::default(),
        )
        .unwrap();
        let id = doc.id.clone();

        for i in 2..=4 {
            let mut update_data = HashMap::new();
            update_data.insert("title".to_string(), format!("V{}", i));
            persist_update(
                &conn,
                "posts",
                &id,
                &def,
                &update_data,
                &HashMap::new(),
                &PersistOptions::default(),
            )
            .unwrap();
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
            )",
        )
        .unwrap();

        let blocks_field = FieldDefinition::builder("content", FieldType::Blocks).build();
        let tabs_field = FieldDefinition::builder("page_settings", FieldType::Tabs)
            .tabs(vec![FieldTab::new("Content", vec![blocks_field])])
            .build();
        let mut def = test_def();
        def.versions = Some(VersionsConfig::new(true, 10));
        def.fields = vec![
            FieldDefinition::builder("title", FieldType::Text).build(),
            tabs_field,
        ];

        let mut data = HashMap::new();
        data.insert("title".to_string(), "Page 1".to_string());
        let doc = persist_create(
            &conn,
            "posts",
            &def,
            &data,
            &HashMap::new(),
            &PersistOptions::default(),
        )
        .unwrap();
        let id = doc.id.clone();

        let mut hook_data: HashMap<String, Value> = HashMap::new();
        hook_data.insert("title".to_string(), json!("Page 1 Draft"));
        hook_data.insert(
            "content".to_string(),
            json!([
                {"_block_type": "hero", "heading": "Welcome"},
                {"_block_type": "text", "body": "Hello world"},
            ]),
        );

        persist_draft_version(&conn, "posts", &id, &def, &hook_data, None).unwrap();

        let versions = query::list_versions(&conn, "posts", &id, None, None).unwrap();
        let draft_version = versions
            .iter()
            .find(|v| v.status == "draft")
            .expect("should have a draft version");
        let snapshot = draft_version.snapshot.as_object().unwrap();
        let content = snapshot
            .get("content")
            .expect("draft snapshot must include blocks from inside Tabs");
        let blocks = content.as_array().unwrap();
        assert_eq!(blocks.len(), 2, "draft snapshot should have 2 blocks");
        assert_eq!(blocks[0]["_block_type"], "hero");
        assert_eq!(blocks[1]["_block_type"], "text");
    }
}
