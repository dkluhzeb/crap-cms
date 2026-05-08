//! Shared service layer for collection/global CRUD operations.
//!
//! These synchronous functions encapsulate the transaction lifecycle (open tx → run hooks →
//! DB operation → commit) shared between admin handlers and the gRPC service. They are meant
//! to be called from within `spawn_blocking`.

pub mod auth;
mod collection;
pub(crate) mod document_info;
mod email;
mod error;
mod globals;
pub(crate) mod helpers;
pub(crate) mod hooks;
pub mod jobs;
mod persist;
pub(crate) mod read;
mod types;
pub mod upload;
pub(crate) mod user_settings;
pub(crate) mod versions;
pub(crate) mod write;

pub use error::ServiceError;
pub(crate) use types::AfterChangeInput;
pub use types::{
    CountDocumentsInput, Def, EmailContext, EventQueue, FindByIdInput, FindDocumentsInput,
    GetGlobalInput, ListVersionsInput, PaginatedResult, PersistOptions, SearchDocumentsInput,
    ServiceContext, VerificationQueue, WriteInput, WriteResult, values_from_strings,
};
pub(crate) use types::{flush_queue, flush_verification_queue};

pub use collection::{
    CreateManyItem, CreateManyOptions, CreateManyResult, DeleteManyOptions, DeleteManyResult,
    UpdateManyOptions, UpdateManyResult, create_document, create_many, delete_document,
    delete_many, undelete_document, unpublish_document, update_document, update_many,
};
pub(crate) use email::send_verification_email;
pub use globals::{unpublish_global_document, update_global_document, update_global_in_conn};
pub(crate) use helpers::run_after_change_hooks;
pub use hooks::{
    LuaReadHooks, LuaWriteHooks, ReadHooks, RunnerReadHooks, RunnerWriteHooks, WriteHooks,
};
pub(crate) use persist::persist_bulk_update;
pub use persist::{persist_create, persist_draft_version, persist_unpublish, persist_update};
pub use read::{
    count_documents, find_document_by_id, find_documents, get_global_document, search_documents,
    validate_access_constraints, validate_user_filters,
};
pub(crate) use versions::unpublish_with_snapshot;
pub use versions::{
    find_version_by_id, list_versions, restore_collection_version, restore_global_version,
};
pub use write::{ValidateContext, create_document_in_conn, validate_document};
pub(crate) use write::{
    delete_document_in_conn, update_document_in_conn, update_many_single_in_conn,
};

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    use crate::core::DocumentFields;
    use crate::core::collection::*;
    use crate::core::field::*;
    use rusqlite::Connection;
    use serde_json::json;

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
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Hello"));

        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .build();

        let doc = persist_create(&ctx, &data, &PersistOptions::default()).unwrap();
        assert!(!doc.id.is_empty());
        assert_eq!(doc.get_str("title"), Some("Hello"));
    }

    #[test]
    fn persist_update_basic() {
        let conn = setup_db();
        let def = test_def();
        let mut data = DocumentFields::new();
        data.insert("title".to_string(), json!("Original"));

        let ctx = ServiceContext::collection("posts", &def)
            .conn(&conn)
            .build();

        let doc = persist_create(&ctx, &data, &PersistOptions::default()).unwrap();
        let id = doc.id.clone();

        let mut update_data = DocumentFields::new();
        update_data.insert("title".to_string(), json!("Updated"));

        let updated = persist_update(&ctx, &id, &update_data, &PersistOptions::default()).unwrap();
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

        let mut data = DocumentFields::new();
        data.insert("alt".to_string(), json!("Test Image"));
        data.insert("filename".to_string(), json!("abc123_test.jpg"));
        data.insert("mime_type".to_string(), json!("image/jpeg"));
        data.insert("filesize".to_string(), json!("12345"));
        data.insert("width".to_string(), json!("1920"));
        data.insert("height".to_string(), json!("1080"));
        data.insert("url".to_string(), json!("/uploads/media/abc123_test.jpg"));

        let ctx = ServiceContext::collection("media", &def)
            .conn(&conn)
            .build();

        let doc = persist_create(&ctx, &data, &PersistOptions::default()).unwrap();

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
