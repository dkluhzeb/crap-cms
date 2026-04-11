//! Shared service layer for collection/global CRUD operations.
//!
//! These synchronous functions encapsulate the transaction lifecycle (open tx → run hooks →
//! DB operation → commit) shared between admin handlers and the gRPC service. They are meant
//! to be called from within `spawn_blocking`.

pub mod auth;
mod collection;
pub mod document_info;
mod email;
mod error;
mod globals;
pub mod helpers;
pub mod hooks;
pub mod jobs;
mod persist;
pub mod read;
mod types;
pub mod upload;
pub mod user_settings;
pub(crate) mod versions;
pub mod write;

pub use error::ServiceError;
pub(crate) use types::AfterChangeInput;
pub use types::{
    CountDocumentsInput, CountDocumentsInputBuilder, FindByIdInput, FindByIdInputBuilder,
    FindDocumentsInput, FindDocumentsInputBuilder, GetGlobalInput, ListVersionsInput,
    PaginatedResult, PersistOptions, PersistOptionsBuilder, SearchDocumentsInput, ServiceContext,
    ServiceContextBuilder, WriteInput, WriteInputBuilder, WriteResult,
};

pub use collection::{
    create_document, delete_document, undelete_document, undelete_document_core,
    unpublish_document, unpublish_document_core, update_document,
};
pub use email::send_verification_email;
pub use globals::{unpublish_global_document, update_global_core, update_global_document};
pub(crate) use helpers::{build_hook_data, run_after_change_hooks};
pub use hooks::{
    LuaReadHooks, LuaReadHooksBuilder, LuaWriteHooks, LuaWriteHooksBuilder, ReadHooks,
    RunnerReadHooks, RunnerWriteHooks, WriteHooks,
};
pub use persist::{
    persist_bulk_update, persist_create, persist_draft_version, persist_unpublish, persist_update,
};
pub use read::{
    ReadOptions, ReadOptionsBuilder, count_documents, find_document_by_id, find_documents,
    get_global_document, search_documents,
};
pub(crate) use versions::restore_collection_version_core;
pub use versions::unpublish_with_snapshot;
pub use versions::{
    find_version_by_id, list_versions, restore_collection_version, restore_global_version,
};
pub use write::{
    DeleteResult, ValidateContext, create_document_core, delete_document_core,
    update_document_core, update_many_single_core, validate_document,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::collection::*;
    use crate::core::field::*;
    use rusqlite::Connection;
    use std::collections::HashMap;

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

        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .build();

        let doc = persist_create(&ctx, &data, &HashMap::new(), &PersistOptions::default()).unwrap();
        assert!(!doc.id.is_empty());
        assert_eq!(doc.get_str("title"), Some("Hello"));
    }

    #[test]
    fn persist_update_basic() {
        let conn = setup_db();
        let def = test_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Original".to_string());

        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .build();

        let doc = persist_create(&ctx, &data, &HashMap::new(), &PersistOptions::default()).unwrap();
        let id = doc.id.clone();

        let mut update_data = HashMap::new();
        update_data.insert("title".to_string(), "Updated".to_string());

        let updated = persist_update(
            &ctx,
            &id,
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

        let ctx = ServiceContext::collection("media", &def)
            .conn(&conn)
            .build();

        let doc = persist_create(&ctx, &data, &HashMap::new(), &PersistOptions::default()).unwrap();

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
