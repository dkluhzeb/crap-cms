//! Shared service layer for collection/global CRUD operations.
//!
//! These synchronous functions encapsulate the transaction lifecycle (open tx → run hooks →
//! DB operation → commit) shared between admin handlers and the gRPC service. They are meant
//! to be called from within `spawn_blocking`.

mod collections;
mod email;
mod globals;
mod versions;

pub use collections::{create_document, delete_document, unpublish_document, update_document};
pub use email::send_verification_email;
pub use globals::update_global_document;

use std::collections::HashMap;

use anyhow::Result;

use crate::core::document::Document;
use crate::core::field::FieldDefinition;
use crate::core::CollectionDefinition;
use crate::db::query::{self, LocaleContext};
use crate::hooks::lifecycle::{HookContext, HookEvent, HookRunner};

/// Validate a password against the configured policy. Call this before any
/// password-setting operation (create, update, reset).
pub fn validate_password(password: &str, policy: &crate::config::PasswordPolicy) -> Result<()> {
    policy.validate(password)
}

/// Result of a write operation: the document and the request-scoped hook context.
pub type WriteResult = (Document, HashMap<String, serde_json::Value>);

/// Input data for a write operation (create/update). Bundles the 6 data parameters
/// that callers provide, reducing argument count on public API functions.
pub struct WriteInput<'a> {
    pub data: HashMap<String, String>,
    pub join_data: &'a HashMap<String, serde_json::Value>,
    pub password: Option<&'a str>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub locale: Option<String>,
    pub draft: bool,
    pub ui_locale: Option<String>,
}

/// Build the hook data map from form data + structured join data.
/// Converts string values to JSON strings and merges in blocks/arrays/has-many.
pub(crate) fn build_hook_data(
    data: &HashMap<String, String>,
    join_data: &HashMap<String, serde_json::Value>,
) -> HashMap<String, serde_json::Value> {
    let mut hook_data: HashMap<String, serde_json::Value> = data
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    for (k, v) in join_data {
        hook_data.insert(k.clone(), v.clone());
    }
    hook_data
}

/// Build a HookContext for a before-write hook invocation.
pub(crate) fn build_before_ctx(
    slug: &str,
    operation: &str,
    hook_data: HashMap<String, serde_json::Value>,
    locale: Option<String>,
    is_draft: bool,
    user: Option<&Document>,
    ui_locale: Option<&str>,
) -> HookContext {
    let mut builder = HookContext::builder(slug, operation)
        .data(hook_data)
        .draft(is_draft)
        .user(user)
        .ui_locale(ui_locale);
    if let Some(l) = locale {
        builder = builder.locale(l);
    }
    builder.build()
}

/// Run after-change hooks and return the request-scoped context.
/// This pattern is repeated across create, update, unpublish, and global update.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_after_change_hooks(
    runner: &HookRunner,
    hooks: &crate::core::collection::Hooks,
    fields: &[FieldDefinition],
    slug: &str,
    operation: &str,
    doc: &Document,
    locale: Option<String>,
    is_draft: bool,
    req_context: HashMap<String, serde_json::Value>,
    tx: &rusqlite::Connection,
    user: Option<&Document>,
    ui_locale: Option<&str>,
) -> Result<HashMap<String, serde_json::Value>> {
    let mut after_data = doc.fields.clone();
    after_data.insert("id".to_string(), serde_json::Value::String(doc.id.clone()));
    let mut builder = HookContext::builder(slug, operation)
        .data(after_data)
        .draft(is_draft)
        .context(req_context)
        .user(user)
        .ui_locale(ui_locale);
    if let Some(l) = locale {
        builder = builder.locale(l);
    }
    let after_ctx = builder.build();
    let after_result = runner.run_after_write(
        hooks,
        fields,
        HookEvent::AfterChange,
        after_ctx,
        tx,
        user,
        ui_locale,
    )?;
    Ok(after_result.context)
}

/// Persist the DB write phase of a create operation.
/// Performs: insert → join data → password → version snapshot.
pub fn persist_create(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    final_data: &HashMap<String, String>,
    hook_data: &HashMap<String, serde_json::Value>,
    password: Option<&str>,
    locale_ctx: Option<&LocaleContext>,
    is_draft: bool,
) -> Result<Document> {
    let status = if is_draft { "draft" } else { "published" };
    let doc = query::create(conn, slug, def, final_data, locale_ctx)?;
    query::save_join_table_data(conn, slug, &def.fields, &doc.id, hook_data, locale_ctx)?;

    if let Some(pw) = password {
        if !pw.is_empty() {
            query::update_password(conn, slug, &doc.id, pw)?;
        }
    }

    if def.has_versions() {
        versions::create_version_snapshot(
            conn,
            slug,
            &doc.id,
            &def.fields,
            def.versions.as_ref(),
            def.has_drafts(),
            status,
            &doc,
        )?;
    }

    query::fts::fts_upsert(conn, slug, &doc, Some(def))?;
    Ok(doc)
}

/// Persist the DB write phase of a normal (non-draft) update operation.
/// Performs: update → join data → password → version snapshot (published).
#[allow(clippy::too_many_arguments)]
pub fn persist_update(
    conn: &rusqlite::Connection,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    final_data: &HashMap<String, String>,
    hook_data: &HashMap<String, serde_json::Value>,
    password: Option<&str>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let doc = query::update(conn, slug, def, id, final_data, locale_ctx)?;
    query::save_join_table_data(conn, slug, &def.fields, &doc.id, hook_data, locale_ctx)?;

    if let Some(pw) = password {
        if !pw.is_empty() {
            query::update_password(conn, slug, &doc.id, pw)?;
        }
    }

    if def.has_versions() {
        versions::create_version_snapshot(
            conn,
            slug,
            &doc.id,
            &def.fields,
            def.versions.as_ref(),
            def.has_drafts(),
            "published",
            &doc,
        )?;
    }

    query::fts::fts_upsert(conn, slug, &doc, Some(def))?;
    Ok(doc)
}

/// Persist a draft-only version save: find existing doc, merge incoming data,
/// create a draft version snapshot. Main table is NOT modified.
pub fn persist_draft_version(
    conn: &rusqlite::Connection,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    hook_data: &HashMap<String, serde_json::Value>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let existing_doc = query::find_by_id_raw(conn, slug, def, id, locale_ctx)?
        .ok_or_else(|| anyhow::anyhow!("Document {} not found in {}", id, slug))?;

    versions::save_draft_version(
        conn,
        slug,
        id,
        &def.fields,
        def.versions.as_ref(),
        &existing_doc,
        hook_data,
    )?;

    Ok(existing_doc)
}

/// Persist an unpublish operation: find existing doc, set status to draft,
/// create a draft version snapshot. Returns the existing doc.
pub fn persist_unpublish(
    conn: &rusqlite::Connection,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
) -> Result<Document> {
    let doc = query::find_by_id_raw(conn, slug, def, id, None)?
        .ok_or_else(|| anyhow::anyhow!("Document {} not found in {}", id, slug))?;

    query::set_document_status(conn, slug, id, "draft")?;
    let snapshot = query::build_snapshot(conn, slug, &def.fields, &doc)?;
    query::create_version(conn, slug, id, "draft", &snapshot)?;
    versions::prune_versions(conn, slug, id, def.versions.as_ref())?;

    Ok(doc)
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
            None,
            None,
            false,
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
            None,
            None,
            false,
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
            None,
            None,
        )
        .unwrap();
        assert_eq!(updated.get_str("title"), Some("Updated"));
    }

    #[test]
    fn persist_create_with_upload_metadata() {
        let conn = Connection::open_in_memory().unwrap();

        let mut fields = vec![FieldDefinition::builder("alt", FieldType::Text)
            .required(true)
            .build()];

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
            None,
            None,
            false,
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
