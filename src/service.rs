//! Shared service layer for collection/global CRUD operations.
//!
//! These synchronous functions encapsulate the transaction lifecycle (open tx → run hooks →
//! DB operation → commit) shared between admin handlers and the gRPC service. They are meant
//! to be called from within `spawn_blocking`.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context as _, Result};

use crate::config::{EmailConfig, ServerConfig};
use crate::core::collection::{GlobalDefinition, VersionsConfig};
use crate::core::document::Document;
use crate::core::email::EmailRenderer;
use crate::core::field::FieldDefinition;
use crate::core::CollectionDefinition;
use crate::db::query::{self, LocaleContext};
use crate::db::DbPool;
use crate::hooks::lifecycle::{self, HookContext, HookEvent, HookRunner};

/// Result of a write operation: the document and the request-scoped hook context.
pub type WriteResult = (Document, HashMap<String, serde_json::Value>);

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
/// create a version snapshot, and prune. `final_ctx_data` contains all hook-modified
/// data including join table fields (arrays, blocks, relationships).
fn save_draft_version(
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
    // Merge incoming hook-modified join data (blocks/arrays/has-many) into the snapshot.
    // build_snapshot hydrates from join tables (which have the old/published data),
    // so we must overwrite with the hook-processed data for draft-only saves.
    if let Some(obj) = snapshot.as_object_mut() {
        merge_join_data_into_snapshot(obj, fields, final_ctx_data);
    }
    query::create_version(conn, table, parent_id, "draft", &snapshot)?;
    if let Some(vc) = versions {
        if vc.max_versions > 0 {
            query::prune_versions(conn, table, parent_id, vc.max_versions)?;
        }
    }
    Ok(())
}

/// Set document status, create a version snapshot, and prune.
/// Used for both initial creates (status may be "draft") and normal updates ("published").
fn create_version_snapshot(
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
    if let Some(vc) = versions {
        if vc.max_versions > 0 {
            query::prune_versions(conn, table, parent_id, vc.max_versions)?;
        }
    }
    Ok(())
}

/// Persist the DB write phase of a create operation.
/// Performs: insert → join data → password → version snapshot.
/// Called by both `create_document` (service layer) and Lua CRUD.
#[allow(clippy::too_many_arguments)]
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
        create_version_snapshot(
            conn, slug, &doc.id, &def.fields,
            def.versions.as_ref(), def.has_drafts(), status, &doc,
        )?;
    }

    Ok(doc)
}

/// Persist the DB write phase of a normal (non-draft) update operation.
/// Performs: update → join data → password → version snapshot (published).
/// Called by both `update_document` (service layer) and Lua CRUD.
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
        create_version_snapshot(
            conn, slug, &doc.id, &def.fields,
            def.versions.as_ref(), def.has_drafts(), "published", &doc,
        )?;
    }

    Ok(doc)
}

/// Persist a draft-only version save: find existing doc, merge incoming data,
/// create a draft version snapshot. Main table is NOT modified.
/// Called by both `update_document` (draft path) and Lua CRUD.
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

    save_draft_version(
        conn, slug, id, &def.fields, def.versions.as_ref(),
        &existing_doc, hook_data,
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
    if let Some(ref vc) = def.versions {
        if vc.max_versions > 0 {
            query::prune_versions(conn, slug, id, vc.max_versions)?;
        }
    }

    Ok(doc)
}

/// Create a document within a single transaction: before-hooks → insert → join data → password.
/// When `draft` is true and the collection has drafts enabled, the document is created with
/// `_status = 'draft'` and required-field validation is skipped.
/// Returns the created document and the request-scoped context from before-hooks.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
#[allow(clippy::too_many_arguments)]
pub fn create_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    def: &CollectionDefinition,
    data: HashMap<String, String>,
    join_data: &HashMap<String, serde_json::Value>,
    password: Option<&str>,
    locale_ctx: Option<&LocaleContext>,
    locale: Option<String>,
    user: Option<&Document>,
    draft: bool,
) -> Result<WriteResult> {
    let is_draft = draft && def.has_drafts();

    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .context("Start transaction")?;

    let mut hook_data: HashMap<String, serde_json::Value> = data.iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    // Merge structured join data (blocks, arrays, has-many) so validation and hooks
    // see them as proper JSON arrays rather than flat bracket-notation keys.
    for (k, v) in join_data {
        hook_data.insert(k.clone(), v.clone());
    }
    let hook_ctx = HookContext {
        collection: slug.to_string(),
        operation: "create".to_string(),
        data: hook_data,
        locale: locale.clone(),
        draft: Some(is_draft),
        context: HashMap::new(),
    };
    let final_ctx = runner.run_before_write(
        &def.hooks, &def.fields, hook_ctx, &tx, slug, None, user, is_draft,
    )?;
    let req_context = final_ctx.context.clone();
    let final_data = lifecycle::hook_ctx_to_string_map(&final_ctx, &def.fields);
    let doc = persist_create(
        &tx, slug, def, &final_data, &final_ctx.data,
        password, locale_ctx, is_draft,
    )?;

    // After-hooks: run inside the same transaction, with CRUD access
    let mut after_data = doc.fields.clone();
    after_data.insert("id".to_string(), serde_json::Value::String(doc.id.clone()));
    let after_ctx = HookContext {
        collection: slug.to_string(),
        operation: "create".to_string(),
        data: after_data,
        locale,
        draft: Some(is_draft),
        context: req_context,
    };
    let after_result = runner.run_after_write(
        &def.hooks, &def.fields, HookEvent::AfterChange,
        after_ctx, &tx, user,
    )?;
    let req_context = after_result.context;

    tx.commit().context("Commit transaction")?;
    Ok((doc, req_context))
}

/// Update a document within a single transaction: before-hooks → update → join data → password.
/// When `draft` is true and the collection has drafts enabled, the update creates a version-only
/// save: the main table is NOT modified, only a new version snapshot is recorded. On publish
/// (`draft=false`), the main table is updated and `_status` set to `"published"`.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
#[allow(clippy::too_many_arguments)]
pub fn update_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    data: HashMap<String, String>,
    join_data: &HashMap<String, serde_json::Value>,
    password: Option<&str>,
    locale_ctx: Option<&LocaleContext>,
    locale: Option<String>,
    user: Option<&Document>,
    draft: bool,
) -> Result<WriteResult> {
    let is_draft = draft && def.has_drafts();

    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .context("Start transaction")?;

    let mut hook_data: HashMap<String, serde_json::Value> = data.iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    // Merge structured join data (blocks, arrays, has-many) so validation and hooks
    // see them as proper JSON arrays rather than flat bracket-notation keys.
    for (k, v) in join_data {
        hook_data.insert(k.clone(), v.clone());
    }
    let hook_ctx = HookContext {
        collection: slug.to_string(),
        operation: "update".to_string(),
        data: hook_data,
        locale: locale.clone(),
        draft: Some(is_draft),
        context: HashMap::new(),
    };
    let final_ctx = runner.run_before_write(
        &def.hooks, &def.fields, hook_ctx, &tx, slug, Some(id), user, is_draft,
    )?;
    let req_context = final_ctx.context.clone();
    let final_data = lifecycle::hook_ctx_to_string_map(&final_ctx, &def.fields);

    if is_draft && def.has_versions() {
        // Version-only save: do NOT update the main table.
        let existing_doc = persist_draft_version(
            &tx, slug, id, def, &final_ctx.data, locale_ctx,
        )?;

        // After-hooks: run inside the same transaction, with CRUD access
        let mut after_data = existing_doc.fields.clone();
        after_data.insert("id".to_string(), serde_json::Value::String(existing_doc.id.clone()));
        let after_ctx = HookContext {
            collection: slug.to_string(),
            operation: "update".to_string(),
            data: after_data,
            locale: locale.clone(),
            draft: Some(is_draft),
            context: req_context,
        };
        let after_result = runner.run_after_write(
            &def.hooks, &def.fields, HookEvent::AfterChange,
            after_ctx, &tx, user,
        )?;
        let req_context = after_result.context;

        tx.commit().context("Commit transaction")?;
        Ok((existing_doc, req_context))
    } else {
        // Normal update: write to main table
        let doc = persist_update(
            &tx, slug, id, def, &final_data, &final_ctx.data,
            password, locale_ctx,
        )?;

        // After-hooks: run inside the same transaction, with CRUD access
        let mut after_data = doc.fields.clone();
        after_data.insert("id".to_string(), serde_json::Value::String(doc.id.clone()));
        let after_ctx = HookContext {
            collection: slug.to_string(),
            operation: "update".to_string(),
            data: after_data,
            locale: locale.clone(),
            draft: Some(is_draft),
            context: req_context,
        };
        let after_result = runner.run_after_write(
            &def.hooks, &def.fields, HookEvent::AfterChange,
            after_ctx, &tx, user,
        )?;
        let req_context = after_result.context;

        tx.commit().context("Commit transaction")?;
        Ok((doc, req_context))
    }
}

/// Unpublish a versioned document: set status to draft, create a version snapshot,
/// and run before/after change hooks. Returns the document.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
#[allow(clippy::too_many_arguments)]
pub fn unpublish_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    user: Option<&Document>,
) -> Result<Document> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .context("Start transaction")?;

    let doc = query::find_by_id_raw(&tx, slug, def, id, None)?
        .ok_or_else(|| anyhow::anyhow!("Document {} not found in {}", id, slug))?;

    // Run before_change hooks (unpublish is a state change)
    let hook_ctx = HookContext {
        collection: slug.to_string(),
        operation: "update".to_string(),
        data: doc.fields.clone(),
        locale: None,
        draft: Some(false),
        context: HashMap::new(),
    };
    let final_ctx = runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, hook_ctx, &tx, user)?;

    persist_unpublish(&tx, slug, id, def)?;

    // Run after_change hooks
    let mut after_data = doc.fields.clone();
    after_data.insert("id".to_string(), serde_json::Value::String(id.to_string()));
    let after_ctx = HookContext {
        collection: slug.to_string(),
        operation: "update".to_string(),
        data: after_data,
        locale: None,
        draft: Some(false),
        context: final_ctx.context,
    };
    runner.run_hooks_with_conn(&def.hooks, HookEvent::AfterChange, after_ctx, &tx, user)?;

    tx.commit().context("Commit transaction")?;
    Ok(doc)
}

/// Delete a document within a single transaction: before-hooks → delete → upload cleanup.
/// Returns the request-scoped context from before-hooks.
/// If `config_dir` is provided and the collection is an upload collection,
/// upload files are cleaned up after successful deletion.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
#[allow(clippy::too_many_arguments)]
pub fn delete_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    user: Option<&Document>,
    config_dir: Option<&std::path::Path>,
) -> Result<HashMap<String, serde_json::Value>> {
    let mut conn = pool.get().context("DB connection")?;

    // For upload collections, load the document before deleting to get file paths
    let upload_doc_fields = if def.is_upload_collection() {
        let locale_ctx = query::LocaleContext::from_locale_string(None, &crate::config::LocaleConfig::default());
        query::find_by_id(&conn, slug, def, id, locale_ctx.as_ref())
            .ok()
            .flatten()
            .map(|doc| doc.fields.clone())
    } else {
        None
    };

    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .context("Start transaction")?;

    let hook_ctx = HookContext {
        collection: slug.to_string(),
        operation: "delete".to_string(),
        data: [("id".to_string(), serde_json::Value::String(id.to_string()))].into(),
        locale: None,
        draft: None,
        context: HashMap::new(),
    };
    let final_ctx = runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeDelete, hook_ctx, &tx, user)?;
    query::delete(&tx, slug, id)?;

    // After-hooks: run inside the same transaction, with CRUD access
    let after_ctx = HookContext {
        collection: slug.to_string(),
        operation: "delete".to_string(),
        data: [("id".to_string(), serde_json::Value::String(id.to_string()))].into(),
        locale: None,
        draft: None,
        context: final_ctx.context,
    };
    let after_result = runner.run_hooks_with_conn(&def.hooks, HookEvent::AfterDelete, after_ctx, &tx, user)?;

    tx.commit().context("Commit transaction")?;

    // Clean up upload files after successful commit
    if let (Some(dir), Some(fields)) = (config_dir, upload_doc_fields) {
        crate::core::upload::delete_upload_files(dir, &fields);
    }

    Ok(after_result.context)
}

/// Update a global document within a single transaction: before-hooks → update → join data.
/// When `draft` is true and the global has drafts enabled, creates a version-only save
/// (main table NOT modified). On publish (`draft=false`), the main table is updated.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
#[allow(clippy::too_many_arguments)]
pub fn update_global_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    def: &GlobalDefinition,
    data: HashMap<String, String>,
    join_data: &HashMap<String, serde_json::Value>,
    locale_ctx: Option<&LocaleContext>,
    locale: Option<String>,
    user: Option<&Document>,
    draft: bool,
) -> Result<WriteResult> {
    let is_draft = draft && def.has_drafts();

    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .context("Start transaction")?;

    let global_table = format!("_global_{}", slug);

    let mut hook_data: HashMap<String, serde_json::Value> = data.iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect();
    // Merge structured join data (blocks, arrays, has-many) so validation and hooks
    // see them as proper JSON arrays rather than flat bracket-notation keys.
    for (k, v) in join_data {
        hook_data.insert(k.clone(), v.clone());
    }
    let hook_ctx = HookContext {
        collection: slug.to_string(),
        operation: "update".to_string(),
        data: hook_data,
        locale: locale.clone(),
        draft: Some(is_draft),
        context: HashMap::new(),
    };
    let final_ctx = runner.run_before_write(
        &def.hooks, &def.fields, hook_ctx, &tx, &global_table, Some("default"), user, is_draft,
    )?;
    let req_context = final_ctx.context.clone();
    let final_data = lifecycle::hook_ctx_to_string_map(&final_ctx, &def.fields);

    if is_draft && def.has_versions() {
        // Version-only save: do NOT update the main table.
        let existing_doc = query::get_global(&tx, slug, def, locale_ctx)?;

        save_draft_version(
            &tx, &global_table, "default", &def.fields, def.versions.as_ref(),
            &existing_doc, &final_ctx.data,
        )?;

        // After-hooks
        let mut after_data = existing_doc.fields.clone();
        after_data.insert("id".to_string(), serde_json::Value::String(existing_doc.id.clone()));
        let after_ctx = HookContext {
            collection: slug.to_string(),
            operation: "update".to_string(),
            data: after_data,
            locale: locale.clone(),
            draft: Some(is_draft),
            context: req_context,
        };
        let after_result = runner.run_after_write(
            &def.hooks, &def.fields, HookEvent::AfterChange,
            after_ctx, &tx, user,
        )?;
        let req_context = after_result.context;

        tx.commit().context("Commit transaction")?;
        Ok((existing_doc, req_context))
    } else {
        // Normal update: write to main table
        let doc = query::update_global(&tx, slug, def, &final_data, locale_ctx)?;

        // Use hook-modified data so before_change hooks that alter arrays/blocks/relationships
        // have their changes persisted.
        query::save_join_table_data(&tx, &global_table, &def.fields, "default", &final_ctx.data, locale_ctx)?;

        // Versioning: set status to published and create version
        if def.has_versions() {
            create_version_snapshot(
                &tx, &global_table, "default", &def.fields,
                def.versions.as_ref(), def.has_drafts(), "published", &doc,
            )?;
        }

        // After-hooks: run inside the same transaction, with CRUD access
        let mut after_data = doc.fields.clone();
        after_data.insert("id".to_string(), serde_json::Value::String(doc.id.clone()));
        let after_ctx = HookContext {
            collection: slug.to_string(),
            operation: "update".to_string(),
            data: after_data,
            locale,
            draft: Some(is_draft),
            context: req_context,
        };
        let after_result = runner.run_after_write(
            &def.hooks, &def.fields, HookEvent::AfterChange,
            after_ctx, &tx, user,
        )?;
        let req_context = after_result.context;

        tx.commit().context("Commit transaction")?;
        Ok((doc, req_context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use crate::core::collection::*;
    use crate::core::field::*;

    fn test_def() -> CollectionDefinition {
        CollectionDefinition {
            slug: "posts".to_string(),
            labels: CollectionLabels::default(),
            timestamps: true,
            fields: vec![
                FieldDefinition {
                    name: "title".to_string(),
                    ..Default::default()
                },
            ],
            admin: CollectionAdmin::default(),
            hooks: CollectionHooks::default(),
            auth: None,
            upload: None,
            access: CollectionAccess::default(),
            live: None,
            versions: None,
        }
    }

    fn versioned_def() -> CollectionDefinition {
        CollectionDefinition {
            versions: Some(VersionsConfig { drafts: true, max_versions: 10 }),
            ..test_def()
        }
    }

    fn setup_db(has_versions: bool) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE posts (
                id TEXT PRIMARY KEY,
                title TEXT,
                _status TEXT DEFAULT 'published',
                created_at TEXT,
                updated_at TEXT
            )"
        ).unwrap();
        if has_versions {
            conn.execute_batch(
                "CREATE TABLE _versions_posts (
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
        }
        conn
    }

    #[test]
    fn persist_create_basic() {
        let conn = setup_db(false);
        let def = test_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Hello".to_string());
        let hook_data = HashMap::new();

        let doc = persist_create(&conn, "posts", &def, &data, &hook_data, None, None, false).unwrap();
        assert!(!doc.id.is_empty());
        assert_eq!(doc.get_str("title"), Some("Hello"));
    }

    #[test]
    fn persist_create_with_versions() {
        let conn = setup_db(true);
        let def = versioned_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Versioned".to_string());
        let hook_data = HashMap::new();

        let doc = persist_create(&conn, "posts", &def, &data, &hook_data, None, None, false).unwrap();
        assert_eq!(doc.get_str("title"), Some("Versioned"));

        // Should have created a version snapshot
        let count = query::count_versions(&conn, "posts", &doc.id).unwrap();
        assert_eq!(count, 1, "should have 1 version after create");
    }

    #[test]
    fn persist_create_draft() {
        let conn = setup_db(true);
        let def = versioned_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Draft Post".to_string());
        let hook_data = HashMap::new();

        let doc = persist_create(&conn, "posts", &def, &data, &hook_data, None, None, true).unwrap();
        assert_eq!(doc.get_str("title"), Some("Draft Post"));

        // Should have created a draft version
        let status = query::get_document_status(&conn, "posts", &doc.id).unwrap();
        assert_eq!(status.as_deref(), Some("draft"));
    }

    #[test]
    fn persist_update_basic() {
        let conn = setup_db(false);
        let def = test_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Original".to_string());
        let hook_data = HashMap::new();

        let doc = persist_create(&conn, "posts", &def, &data, &hook_data, None, None, false).unwrap();
        let id = doc.id.clone();

        let mut update_data = HashMap::new();
        update_data.insert("title".to_string(), "Updated".to_string());
        let update_hook_data = HashMap::new();

        let updated = persist_update(&conn, "posts", &id, &def, &update_data, &update_hook_data, None, None).unwrap();
        assert_eq!(updated.get_str("title"), Some("Updated"));
    }

    #[test]
    fn persist_update_with_versions() {
        let conn = setup_db(true);
        let def = versioned_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "V1".to_string());
        let hook_data = HashMap::new();

        let doc = persist_create(&conn, "posts", &def, &data, &hook_data, None, None, false).unwrap();
        let id = doc.id.clone();

        let mut update_data = HashMap::new();
        update_data.insert("title".to_string(), "V2".to_string());
        let update_hook_data = HashMap::new();

        persist_update(&conn, "posts", &id, &def, &update_data, &update_hook_data, None, None).unwrap();

        // Should have 2 versions now (create + update)
        let count = query::count_versions(&conn, "posts", &id).unwrap();
        assert_eq!(count, 2, "should have 2 versions after create + update");
    }

    #[test]
    fn persist_draft_version_does_not_modify_main_table() {
        let conn = setup_db(true);
        let def = versioned_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Published".to_string());
        let hook_data = HashMap::new();

        let doc = persist_create(&conn, "posts", &def, &data, &hook_data, None, None, false).unwrap();
        let id = doc.id.clone();

        // Save a draft version with different data
        let mut draft_data = HashMap::new();
        draft_data.insert("title".to_string(), serde_json::json!("Draft Title"));

        let existing = persist_draft_version(&conn, "posts", &id, &def, &draft_data, None).unwrap();
        // persist_draft_version returns the existing (unchanged) doc
        assert_eq!(existing.get_str("title"), Some("Published"), "main table should not be modified");

        // But there should be a new draft version
        let count = query::count_versions(&conn, "posts", &id).unwrap();
        assert_eq!(count, 2, "should have 2 versions (create published + draft)");
    }

    #[test]
    fn persist_unpublish_sets_draft_status() {
        let conn = setup_db(true);
        let def = versioned_def();
        let mut data = HashMap::new();
        data.insert("title".to_string(), "To Unpublish".to_string());
        let hook_data = HashMap::new();

        let doc = persist_create(&conn, "posts", &def, &data, &hook_data, None, None, false).unwrap();
        let id = doc.id.clone();

        // Verify it's published
        let status = query::get_document_status(&conn, "posts", &id).unwrap();
        assert_eq!(status.as_deref(), Some("published"));

        // Unpublish
        let result = persist_unpublish(&conn, "posts", &id, &def).unwrap();
        assert_eq!(result.id, id);

        // Should now be draft
        let status = query::get_document_status(&conn, "posts", &id).unwrap();
        assert_eq!(status.as_deref(), Some("draft"));

        // Should have created another version
        let count = query::count_versions(&conn, "posts", &id).unwrap();
        assert_eq!(count, 2, "should have 2 versions (published create + draft unpublish)");
    }

    #[test]
    fn create_version_snapshot_with_pruning() {
        let conn = setup_db(true);
        let def = CollectionDefinition {
            versions: Some(VersionsConfig { drafts: true, max_versions: 2 }),
            ..test_def()
        };
        let hook_data = HashMap::new();

        let mut data = HashMap::new();
        data.insert("title".to_string(), "V1".to_string());
        let doc = persist_create(&conn, "posts", &def, &data, &hook_data, None, None, false).unwrap();
        let id = doc.id.clone();

        // Create more versions to trigger pruning
        for i in 2..=4 {
            let mut update_data = HashMap::new();
            update_data.insert("title".to_string(), format!("V{}", i));
            persist_update(&conn, "posts", &id, &def, &update_data, &HashMap::new(), None, None).unwrap();
        }

        // Should be capped at max_versions=2
        let count = query::count_versions(&conn, "posts", &id).unwrap();
        assert_eq!(count, 2, "versions should be pruned to max_versions=2");
    }

    #[test]
    fn persist_draft_version_includes_blocks_inside_tabs() {
        // Regression: blocks inside Tabs were missing from draft version snapshots
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
            tabs: vec![FieldTab {
                label: "Content".to_string(),
                description: None,
                fields: vec![blocks_field],
            }],
            ..Default::default()
        };
        let def = CollectionDefinition {
            versions: Some(VersionsConfig { drafts: true, max_versions: 10 }),
            fields: vec![
                FieldDefinition { name: "title".to_string(), ..Default::default() },
                tabs_field,
            ],
            ..test_def()
        };

        // Create a published document
        let mut data = HashMap::new();
        data.insert("title".to_string(), "Page 1".to_string());
        let doc = persist_create(&conn, "posts", &def, &data, &HashMap::new(), None, None, false).unwrap();
        let id = doc.id.clone();

        // Save a draft with blocks data
        let mut hook_data: HashMap<String, serde_json::Value> = HashMap::new();
        hook_data.insert("title".to_string(), serde_json::json!("Page 1 Draft"));
        hook_data.insert("content".to_string(), serde_json::json!([
            {"_block_type": "hero", "heading": "Welcome"},
            {"_block_type": "text", "body": "Hello world"},
        ]));

        persist_draft_version(&conn, "posts", &id, &def, &hook_data, None).unwrap();

        // Find the draft version and check its snapshot contains blocks data
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

/// Fire-and-forget: generate a verification token and send the verification email.
/// Spawns its own `spawn_blocking` task internally.
// Excluded from coverage: async tokio task that requires SMTP email transport,
// DB pool, and email renderer — cannot be unit tested without external services.
#[cfg(not(tarpaulin_include))]
pub fn send_verification_email(
    pool: DbPool,
    email_config: EmailConfig,
    email_renderer: Arc<EmailRenderer>,
    server_config: ServerConfig,
    slug: String,
    user_id: String,
    user_email: String,
) {
    tokio::task::spawn_blocking(move || {
        if !crate::core::email::is_configured(&email_config) {
            tracing::warn!("Email not configured — skipping verification email for {}", user_email);
            return;
        }

        let token = nanoid::nanoid!(32);
        let exp = chrono::Utc::now().timestamp() + 86400; // 24 hours

        let conn = match pool.get() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("DB connection for verification token: {}", e);
                return;
            }
        };
        if let Err(e) = query::set_verification_token(&conn, &slug, &user_id, &token, exp) {
            tracing::error!("Failed to set verification token: {}", e);
            return;
        }

        let verify_url = format!(
            "http://{}:{}/admin/verify-email?token={}",
            server_config.host, server_config.admin_port, token
        );
        let data = serde_json::json!({ "verify_url": verify_url });
        let html = match email_renderer.render("verify_email", &data) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("Failed to render verify email template: {}", e);
                return;
            }
        };

        if let Err(e) = crate::core::email::send_email(
            &email_config, &user_email, "Verify your email", &html, None,
        ) {
            tracing::error!("Failed to send verification email: {}", e);
        }
    });
}
