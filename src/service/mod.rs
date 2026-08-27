//! Shared service layer for collection/global CRUD operations.
//!
//! These synchronous functions encapsulate the transaction lifecycle
//! (open tx -> run before hooks -> DB op -> run after hooks -> commit)
//! shared between admin handlers and the gRPC service. They are meant
//! to be called from within `spawn_blocking`.
//!
//! ## Submodule layout
//!
//! - `types/` -- the input/output value types used across the service
//!   layer: `ServiceContext` + builder, `WriteInput`, `WriteResult`,
//!   `PersistOptions`, `Find*Input`, `*Result`, `Def` (collection
//!   vs global tag), `EmailContext`, the event/verification queues.
//! - `collections/` -- public CRUD entry points for collections:
//!   `create_document`, `update_document`, `delete_document`,
//!   `undelete_document`, `unpublish_document`, plus the bulk
//!   variants (`create_many`, `update_many`, `delete_many`).
//! - `globals/` -- global-document equivalents.
//! - `read/` -- query/read helpers shared by both: `find_documents`,
//!   `find_document_by_id`, `count_documents`, `search_documents`,
//!   `get_global_document`.
//! - `persist/` -- the actual `persist_create` / `persist_update` /
//!   `persist_unpublish` / `persist_draft_version` /
//!   `persist_bulk_update` machinery that materializes a write into
//!   the DB once hooks have run.
//! - `write/` -- transaction-agnostic CRUD (`*_in_conn` suffix
//!   indicates the fn expects a connection in `ctx`).
//! - `versions/` -- version listing, restore, and unpublish-with-
//!   snapshot.
//! - `auth/`, `jobs/`, `upload/` -- domain-specific service helpers.
//! - `hooks/` -- read/write `*Hooks` traits + Lua impls invoked from
//!   the service layer.
//! - `email/`, `helpers/`, `user_settings/`, `document_info/` --
//!   support submodules.
//! - `error.rs` -- `ServiceError` enum + classification helpers
//!   (`From<ServiceError> for Status` lives in `api/`).
//!
//! ## Conventions
//!
//! - Public service fns take `(ctx: &ServiceContext, input: ...)`.
//!   Write ops open their own tx via `ctx.pool` + `ctx.runner()`;
//!   Lua-bridged CRUD passes `ctx.conn` + `ctx.write_hooks` directly.
//! - Transaction-agnostic helpers carry the `_in_conn` suffix; they
//!   never open or commit a transaction themselves.
//! - Optional setters on context/option builders take `Option<&T>`
//!   (or `Option<T>` for owned handles) so a caller can pass through
//!   a parent's optional field without `if let Some(x) = ...`.

pub(crate) mod access;
mod app_infra;
pub mod auth;
mod collections;
mod context;
pub(crate) mod document_info;
mod email;
mod error;
pub(crate) mod events;
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

pub(crate) use access::{
    ReadAccessCtx, requested_views, resolve_trash_scope, resolve_view_scope,
    resolve_visibility_filter,
};
pub use app_infra::{AppInfra, AppInfraBuilder, StandaloneInfra};
pub use context::{Def, ServiceContext};
pub use error::ServiceError;
pub(crate) use types::AfterChangeInput;
pub use types::{
    CountDocumentsInput, EmailContext, EventQueue, FindByIdInput, FindDocumentsInput,
    GetGlobalInput, ListVersionsInput, PaginatedResult, PersistOptions, SearchDocumentsInput,
    VerificationQueue, WriteInput, WriteResult, values_from_strings,
};
pub(crate) use types::{flush_queue, flush_verification_queue, invalidate_user_streams_if_auth};

pub use collections::{
    CreateManyItem, CreateManyOptions, CreateManyResult, DeleteManyOptions, DeleteManyResult,
    UpdateManyOptions, UpdateManyResult, create_document, create_many, delete_document,
    delete_many, undelete_document, unpublish_document, update_document, update_many,
};
pub(crate) use email::{VerificationEmailInput, send_verification_email};
pub(crate) use events::{EventAccessInput, EventAccessMap, EventGate, event_op_str};
pub use globals::{unpublish_global_document, update_global_document, update_global_in_conn};
pub(crate) use helpers::run_after_change_hooks;
pub use hooks::{
    LuaReadHooks, LuaWriteHooks, ReadHooks, RunnerReadHooks, RunnerWriteHooks, WriteHooks,
};
pub(crate) use persist::persist_bulk_update;
pub use persist::{persist_create, persist_draft_version, persist_unpublish, persist_update};
pub use read::{
    CollectionStats, collection_stats, count_documents, find_document_by_id, find_documents,
    get_global_document, search_documents, validate_access_constraint_locales,
    validate_access_constraints, validate_user_filters,
};
pub(crate) use versions::unpublish_with_snapshot;
pub use versions::{
    find_version_by_id, list_versions, restore_collection_version, restore_global_version,
};
pub use write::{ValidateContext, create_document_in_conn, validate_document, validate_outcome};
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
            doc.fields.get("width").and_then(serde_json::Value::as_f64),
            Some(1920.0),
            "width should be stored"
        );
        assert_eq!(
            doc.fields.get("height").and_then(serde_json::Value::as_f64),
            Some(1080.0),
            "height should be stored"
        );
        assert_eq!(
            doc.fields
                .get("filesize")
                .and_then(serde_json::Value::as_f64),
            Some(12345.0),
            "filesize should be stored"
        );
    }
}
