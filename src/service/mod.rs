//! Shared service layer for collection/global CRUD operations.
//!
//! These synchronous functions encapsulate the transaction lifecycle (open tx → run hooks →
//! DB operation → commit) shared between admin handlers and the gRPC service. They are meant
//! to be called from within `spawn_blocking`.

mod after_change_input_builder;
mod collections;
mod email;
mod globals;
mod persist;
mod persist_options_builder;
pub mod read;
pub mod read_hooks;
mod types;
mod version_snapshot_ctx_builder;
pub(crate) mod versions;
pub mod write;
pub mod write_hooks;
mod write_input_builder;

pub(crate) use after_change_input_builder::AfterChangeInputBuilder;
pub use persist_options_builder::PersistOptionsBuilder;
pub(crate) use types::AfterChangeInput;
pub use types::{PersistOptions, WriteInput, WriteResult};
pub use write_input_builder::WriteInputBuilder;

pub use collections::{
    create_document, create_document_with_conn, delete_document, delete_document_with_conn,
    restore_document, unpublish_document, update_document, update_document_with_conn,
};
pub use email::send_verification_email;
pub use globals::{unpublish_global_document, update_global_core, update_global_document};
pub use persist::{persist_create, persist_draft_version, persist_unpublish, persist_update};
pub use read::{FindResult, ReadOptions, find_document_by_id, find_documents, get_global_document};
pub use read_hooks::{LuaReadHooks, ReadHooks, RunnerReadHooks};
pub use versions::unpublish_with_snapshot;
pub use write::{DeleteResult, create_document_core, delete_document_core, update_document_core};
pub use write_hooks::{LuaWriteHooks, RunnerWriteHooks, WriteHooks};

use std::collections::HashMap;

use anyhow::Result;

use serde_json::Value;

use crate::{
    core::{Document, FieldDefinition, collection::Hooks},
    db::DbConnection,
    hooks::{HookContext, HookEvent},
};

/// Build the hook data map from form data + structured join data.
/// Converts string values to JSON strings and merges in blocks/arrays/has-many.
pub(crate) fn build_hook_data(
    data: &HashMap<String, String>,
    join_data: &HashMap<String, Value>,
) -> HashMap<String, Value> {
    let mut hook_data: HashMap<String, Value> = data
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect();
    for (k, v) in join_data {
        hook_data.insert(k.clone(), v.clone());
    }
    hook_data
}

/// Run after-change hooks and return the request-scoped context.
/// This pattern is repeated across create, update, unpublish, and global update.
pub(crate) fn run_after_change_hooks(
    write_hooks: &dyn write_hooks::WriteHooks,
    hooks: &Hooks,
    fields: &[FieldDefinition],
    doc: &Document,
    input: AfterChangeInput<'_>,
    tx: &dyn DbConnection,
) -> Result<HashMap<String, Value>> {
    let mut after_data = doc.fields.clone();
    after_data.insert("id".to_string(), Value::String(doc.id.to_string()));
    let after_ctx = HookContext::builder(input.slug, input.operation)
        .data(after_data)
        .draft(input.is_draft)
        .locale(input.locale)
        .context(input.req_context)
        .user(input.user)
        .ui_locale(input.ui_locale)
        .build();
    let after_result =
        write_hooks.run_after_write(hooks, fields, HookEvent::AfterChange, after_ctx, tx)?;
    Ok(after_result.context)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;
    use crate::core::field::*;
    use rusqlite::Connection;

    fn test_def() -> CollectionDefinition {
        let mut def = CollectionDefinition::new("posts");
        def.timestamps = true;
        def.fields = vec![FieldDefinition::builder("title", FieldType::Text).build()];
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
            )",
        )
        .unwrap();
        conn
    }

    #[test]
    fn persist_create_basic() {
        let conn = setup_db();
        let def = test_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Hello".to_string());

        let doc = persist_create(
            &conn,
            "posts",
            &def,
            &data,
            &HashMap::new(),
            &PersistOptions::default(),
        )
        .unwrap();
        assert!(!doc.id.is_empty());
        assert_eq!(doc.get_str("title"), Some("Hello"));
    }

    #[test]
    fn persist_update_basic() {
        let conn = setup_db();
        let def = test_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Original".to_string());

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
        update_data.insert("title".to_string(), "Updated".to_string());

        let updated = persist_update(
            &conn,
            "posts",
            &id,
            &def,
            &update_data,
            &HashMap::new(),
            &PersistOptions::default(),
        )
        .unwrap();
        assert_eq!(updated.get_str("title"), Some("Updated"));
    }

    #[test]
    fn persist_create_with_upload_metadata() {
        let conn = Connection::open_in_memory().unwrap();

        let mut fields = vec![
            FieldDefinition::builder("alt", FieldType::Text)
                .required(true)
                .build(),
        ];

        let upload_fields = vec![
            FieldDefinition::builder("filename", FieldType::Text)
                .required(true)
                .build(),
            FieldDefinition::builder("mime_type", FieldType::Text)
                .admin(FieldAdmin::builder().hidden(true).build())
                .build(),
            FieldDefinition::builder("filesize", FieldType::Number)
                .admin(FieldAdmin::builder().hidden(true).build())
                .build(),
            FieldDefinition::builder("width", FieldType::Number)
                .admin(FieldAdmin::builder().hidden(true).build())
                .build(),
            FieldDefinition::builder("height", FieldType::Number)
                .admin(FieldAdmin::builder().hidden(true).build())
                .build(),
            FieldDefinition::builder("url", FieldType::Text)
                .admin(FieldAdmin::builder().hidden(true).build())
                .build(),
        ];
        for (i, f) in upload_fields.into_iter().enumerate() {
            fields.insert(i, f);
        }

        let mut def = CollectionDefinition::new("media");
        def.timestamps = true;
        def.fields = fields;

        conn.execute_batch(
            "CREATE TABLE media (
                id TEXT PRIMARY KEY,
                filename TEXT NOT NULL,
                mime_type TEXT,
                filesize REAL,
                width REAL,
                height REAL,
                url TEXT,
                alt TEXT NOT NULL,
                created_at TEXT,
                updated_at TEXT
            )",
        )
        .unwrap();

        let mut data = HashMap::new();
        data.insert("alt".to_string(), "Test Image".to_string());
        data.insert("filename".to_string(), "abc123_test.jpg".to_string());
        data.insert("mime_type".to_string(), "image/jpeg".to_string());
        data.insert("filesize".to_string(), "12345".to_string());
        data.insert("width".to_string(), "1920".to_string());
        data.insert("height".to_string(), "1080".to_string());
        data.insert(
            "url".to_string(),
            "/uploads/media/abc123_test.jpg".to_string(),
        );

        let doc = persist_create(
            &conn,
            "media",
            &def,
            &data,
            &HashMap::new(),
            &PersistOptions::default(),
        )
        .unwrap();

        assert_eq!(doc.get_str("filename"), Some("abc123_test.jpg"));
        assert_eq!(
            doc.get_str("mime_type"),
            Some("image/jpeg"),
            "mime_type should be stored"
        );
        assert_eq!(
            doc.get_str("url"),
            Some("/uploads/media/abc123_test.jpg"),
            "url should be stored"
        );
        assert_eq!(
            doc.fields.get("width").and_then(|v| v.as_f64()),
            Some(1920.0),
            "width should be stored"
        );
        assert_eq!(
            doc.fields.get("height").and_then(|v| v.as_f64()),
            Some(1080.0),
            "height should be stored"
        );
        assert_eq!(
            doc.fields.get("filesize").and_then(|v| v.as_f64()),
            Some(12345.0),
            "filesize should be stored"
        );
    }
}
