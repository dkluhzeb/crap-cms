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
pub use versions::unpublish_with_snapshot;

use std::collections::HashMap;

use anyhow::{Result, anyhow};

use serde_json::Value;

use crate::{
    core::{CollectionDefinition, collection::Hooks, document::Document, field::FieldDefinition},
    db::query::{self, LocaleContext},
    hooks::lifecycle::{HookContext, HookEvent, HookRunner},
};

/// Result of a write operation: the document and the request-scoped hook context.
pub type WriteResult = (Document, HashMap<String, Value>);

/// Input data for a write operation (create/update). Bundles the 6 data parameters
/// that callers provide, reducing argument count on public API functions.
pub struct WriteInput<'a> {
    pub data: HashMap<String, String>,
    pub join_data: &'a HashMap<String, Value>,
    pub password: Option<&'a str>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub locale: Option<String>,
    pub draft: bool,
    pub ui_locale: Option<String>,
}

impl<'a> WriteInput<'a> {
    /// Create a builder with the required data and join_data fields.
    pub fn builder(
        data: HashMap<String, String>,
        join_data: &'a HashMap<String, Value>,
    ) -> WriteInputBuilder<'a> {
        WriteInputBuilder::new(data, join_data)
    }
}

/// Builder for [`WriteInput`]. Created via [`WriteInput::builder`].
pub struct WriteInputBuilder<'a> {
    data: HashMap<String, String>,
    join_data: &'a HashMap<String, Value>,
    password: Option<&'a str>,
    locale_ctx: Option<&'a LocaleContext>,
    locale: Option<String>,
    draft: bool,
    ui_locale: Option<String>,
}

impl<'a> WriteInputBuilder<'a> {
    fn new(data: HashMap<String, String>, join_data: &'a HashMap<String, Value>) -> Self {
        Self {
            data,
            join_data,
            password: None,
            locale_ctx: None,
            locale: None,
            draft: false,
            ui_locale: None,
        }
    }

    pub fn password(mut self, password: Option<&'a str>) -> Self {
        self.password = password;
        self
    }

    pub fn locale_ctx(mut self, locale_ctx: Option<&'a LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;
        self
    }

    pub fn locale(mut self, locale: Option<String>) -> Self {
        self.locale = locale;
        self
    }

    pub fn draft(mut self, draft: bool) -> Self {
        self.draft = draft;
        self
    }

    pub fn ui_locale(mut self, ui_locale: Option<String>) -> Self {
        self.ui_locale = ui_locale;
        self
    }

    pub fn build(self) -> WriteInput<'a> {
        WriteInput {
            data: self.data,
            join_data: self.join_data,
            password: self.password,
            locale_ctx: self.locale_ctx,
            locale: self.locale,
            draft: self.draft,
            ui_locale: self.ui_locale,
        }
    }
}

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

/// Bundled parameters for after-change hook invocation.
pub(crate) struct AfterChangeInput<'a> {
    pub slug: &'a str,
    pub operation: &'a str,
    pub locale: Option<String>,
    pub is_draft: bool,
    pub req_context: HashMap<String, Value>,
    pub user: Option<&'a Document>,
    pub ui_locale: Option<&'a str>,
}

impl<'a> AfterChangeInput<'a> {
    /// Create a builder with the required slug and operation.
    pub fn builder(slug: &'a str, operation: &'a str) -> AfterChangeInputBuilder<'a> {
        AfterChangeInputBuilder::new(slug, operation)
    }
}

/// Builder for [`AfterChangeInput`]. Created via [`AfterChangeInput::builder`].
pub(crate) struct AfterChangeInputBuilder<'a> {
    slug: &'a str,
    operation: &'a str,
    locale: Option<String>,
    is_draft: bool,
    req_context: HashMap<String, Value>,
    user: Option<&'a Document>,
    ui_locale: Option<&'a str>,
}

impl<'a> AfterChangeInputBuilder<'a> {
    fn new(slug: &'a str, operation: &'a str) -> Self {
        Self {
            slug,
            operation,
            locale: None,
            is_draft: false,
            req_context: HashMap::new(),
            user: None,
            ui_locale: None,
        }
    }

    pub fn locale(mut self, locale: Option<String>) -> Self {
        self.locale = locale;
        self
    }

    pub fn draft(mut self, is_draft: bool) -> Self {
        self.is_draft = is_draft;
        self
    }

    pub fn req_context(mut self, req_context: HashMap<String, Value>) -> Self {
        self.req_context = req_context;
        self
    }

    pub fn user(mut self, user: Option<&'a Document>) -> Self {
        self.user = user;
        self
    }

    pub fn ui_locale(mut self, ui_locale: Option<&'a str>) -> Self {
        self.ui_locale = ui_locale;
        self
    }

    pub fn build(self) -> AfterChangeInput<'a> {
        AfterChangeInput {
            slug: self.slug,
            operation: self.operation,
            locale: self.locale,
            is_draft: self.is_draft,
            req_context: self.req_context,
            user: self.user,
            ui_locale: self.ui_locale,
        }
    }
}

/// Run after-change hooks and return the request-scoped context.
/// This pattern is repeated across create, update, unpublish, and global update.
pub(crate) fn run_after_change_hooks(
    runner: &HookRunner,
    hooks: &Hooks,
    fields: &[FieldDefinition],
    doc: &Document,
    input: AfterChangeInput<'_>,
    tx: &rusqlite::Connection,
) -> Result<HashMap<String, Value>> {
    let mut after_data = doc.fields.clone();
    after_data.insert("id".to_string(), Value::String(doc.id.clone()));
    let after_ctx = HookContext::builder(input.slug, input.operation)
        .data(after_data)
        .draft(input.is_draft)
        .locale(input.locale)
        .context(input.req_context)
        .user(input.user)
        .ui_locale(input.ui_locale)
        .build();
    let after_result =
        runner.run_after_write(hooks, fields, HookEvent::AfterChange, after_ctx, tx)?;
    Ok(after_result.context)
}

/// Optional parameters for the persist_create operation.
#[derive(Default)]
pub struct PersistOptions<'a> {
    pub password: Option<&'a str>,
    pub locale_ctx: Option<&'a LocaleContext>,
    pub is_draft: bool,
}

impl<'a> PersistOptions<'a> {
    /// Create a builder with all fields defaulted.
    pub fn builder() -> PersistOptionsBuilder<'a> {
        PersistOptionsBuilder::new()
    }
}

/// Builder for [`PersistOptions`]. Created via [`PersistOptions::builder`].
pub struct PersistOptionsBuilder<'a> {
    password: Option<&'a str>,
    locale_ctx: Option<&'a LocaleContext>,
    is_draft: bool,
}

impl<'a> PersistOptionsBuilder<'a> {
    fn new() -> Self {
        Self {
            password: None,
            locale_ctx: None,
            is_draft: false,
        }
    }

    pub fn password(mut self, password: Option<&'a str>) -> Self {
        self.password = password;
        self
    }

    pub fn locale_ctx(mut self, locale_ctx: Option<&'a LocaleContext>) -> Self {
        self.locale_ctx = locale_ctx;
        self
    }

    pub fn draft(mut self, is_draft: bool) -> Self {
        self.is_draft = is_draft;
        self
    }

    pub fn build(self) -> PersistOptions<'a> {
        PersistOptions {
            password: self.password,
            locale_ctx: self.locale_ctx,
            is_draft: self.is_draft,
        }
    }
}

/// Persist the DB write phase of a create operation.
/// Performs: insert → join data → password → version snapshot.
pub fn persist_create(
    conn: &rusqlite::Connection,
    slug: &str,
    def: &CollectionDefinition,
    final_data: &HashMap<String, String>,
    hook_data: &HashMap<String, Value>,
    opts: &PersistOptions<'_>,
) -> Result<Document> {
    let status = if opts.is_draft { "draft" } else { "published" };
    let doc = query::create(conn, slug, def, final_data, opts.locale_ctx)?;
    query::save_join_table_data(conn, slug, &def.fields, &doc.id, hook_data, opts.locale_ctx)?;

    if let Some(pw) = opts.password
        && !pw.is_empty()
    {
        query::update_password(conn, slug, &doc.id, pw)?;
    }

    if def.has_versions() {
        let ctx = versions::VersionSnapshotCtx::builder(slug, &doc.id)
            .fields(&def.fields)
            .versions(def.versions.as_ref())
            .has_drafts(def.has_drafts())
            .build();
        versions::create_version_snapshot(conn, &ctx, status, &doc)?;
    }

    query::fts::fts_upsert(conn, slug, &doc, Some(def))?;
    Ok(doc)
}

/// Persist the DB write phase of a normal (non-draft) update operation.
/// Performs: update → join data → password → version snapshot (published).
pub fn persist_update(
    conn: &rusqlite::Connection,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    final_data: &HashMap<String, String>,
    hook_data: &HashMap<String, Value>,
    opts: &PersistOptions<'_>,
) -> Result<Document> {
    let doc = query::update(conn, slug, def, id, final_data, opts.locale_ctx)?;
    query::save_join_table_data(conn, slug, &def.fields, &doc.id, hook_data, opts.locale_ctx)?;

    if let Some(pw) = opts.password
        && !pw.is_empty()
    {
        query::update_password(conn, slug, &doc.id, pw)?;
    }

    if def.has_versions() {
        let ctx = versions::VersionSnapshotCtx::builder(slug, &doc.id)
            .fields(&def.fields)
            .versions(def.versions.as_ref())
            .has_drafts(def.has_drafts())
            .build();
        versions::create_version_snapshot(conn, &ctx, "published", &doc)?;
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
    hook_data: &HashMap<String, Value>,
    locale_ctx: Option<&LocaleContext>,
) -> Result<Document> {
    let existing_doc = query::find_by_id_raw(conn, slug, def, id, locale_ctx)?
        .ok_or_else(|| anyhow!("Document {} not found in {}", id, slug))?;

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
        .ok_or_else(|| anyhow!("Document {} not found in {}", id, slug))?;

    versions::unpublish_with_snapshot(conn, slug, id, &def.fields, def.versions.as_ref(), &doc)?;

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
