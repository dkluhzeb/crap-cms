//! Collection CRUD orchestration: create, update, unpublish, delete.
//!
//! Each function wraps before-hooks → DB operation → after-hooks in a single transaction.

use std::collections::HashMap;

use anyhow::Context as _;
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, upload, upload::StorageBackend},
    db::{DbConnection, DbPool, query},
    hooks::{HookContext, HookEvent, HookRunner},
    service::{
        AfterChangeInput, RunnerWriteHooks, ServiceError, WriteInput, WriteResult,
        run_after_change_hooks,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Create a document within a single transaction: before-hooks → insert → after-hooks → commit.
/// When `draft` is true and the collection has drafts enabled, the document is created with
/// `_status = 'draft'` and required-field validation is skipped.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
pub fn create_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    def: &CollectionDefinition,
    input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<WriteResult> {
    let mut conn = pool.get().context("DB connection")?;
    create_document_with_conn(&mut conn, runner, slug, def, input, user)
}

/// Like [`create_document`], but accepts an existing connection (avoids a second pool.get()).
pub fn create_document_with_conn(
    conn: &mut crate::db::BoxedConnection,
    runner: &HookRunner,
    slug: &str,
    def: &CollectionDefinition,
    input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<WriteResult> {
    let tx = conn.transaction_immediate().context("Start transaction")?;
    let wh = RunnerWriteHooks {
        runner,
        hooks_enabled: true,
        conn: Some(&tx),
    };
    let result = crate::service::create_document_core(&tx, &wh, slug, def, input, user)?;
    tx.commit().context("Commit transaction")?;
    Ok(result)
}

/// Update a document within a single transaction: before-hooks → update → after-hooks → commit.
/// When `draft` is true and the collection has drafts enabled, the update creates a version-only
/// save: the main table is NOT modified, only a new version snapshot is recorded.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
pub fn update_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<WriteResult> {
    let mut conn = pool.get().context("DB connection")?;
    update_document_with_conn(&mut conn, runner, slug, id, def, input, user)
}

/// Like [`update_document`], but accepts an existing connection.
pub fn update_document_with_conn(
    conn: &mut crate::db::BoxedConnection,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<WriteResult> {
    let tx = conn.transaction_immediate().context("Start transaction")?;
    let wh = RunnerWriteHooks {
        runner,
        hooks_enabled: true,
        conn: Some(&tx),
    };
    let result = crate::service::update_document_core(&tx, &wh, slug, id, def, input, user)?;
    tx.commit().context("Commit transaction")?;
    Ok(result)
}

/// Unpublish a versioned document: before-hooks → set draft status → after-hooks → commit.
// Excluded from coverage: requires HookRunner (Lua VM) for before/after hooks.
// Tested indirectly through CLI integration tests and gRPC API tests.
#[cfg(not(tarpaulin_include))]
pub fn unpublish_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    user: Option<&Document>,
) -> Result<Document> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let doc = query::find_by_id_raw(&tx, slug, def, id, None, false)?
        .ok_or_else(|| ServiceError::NotFound(format!("Document '{id}' not found in '{slug}'")))?;

    let hook_ctx = HookContext::builder(slug, "update")
        .data(doc.fields.clone())
        .draft(true)
        .locale(None::<String>)
        .user(user)
        .build();
    let final_ctx =
        runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, hook_ctx, &tx)?;

    crate::service::persist_unpublish(&tx, slug, id, def)?;

    let wh = RunnerWriteHooks::new(runner);
    run_after_change_hooks(
        &wh,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(slug, "update")
            .req_context(final_ctx.context)
            .user(user)
            .build(),
        &tx,
    )?;

    tx.commit().context("Commit transaction")?;
    Ok(doc)
}

/// Delete a document within a single transaction: before-hooks → delete → after-hooks → commit.
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
    storage: Option<&dyn StorageBackend>,
    locale_config: Option<&LocaleConfig>,
) -> Result<HashMap<String, Value>> {
    let mut conn = pool.get().context("DB connection")?;
    delete_document_with_conn(
        &mut conn,
        runner,
        slug,
        id,
        def,
        user,
        storage,
        locale_config,
    )
}

/// Like [`delete_document`], but accepts an existing connection.
#[allow(clippy::too_many_arguments)]
pub fn delete_document_with_conn(
    conn: &mut crate::db::BoxedConnection,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    user: Option<&Document>,
    storage: Option<&dyn StorageBackend>,
    locale_config: Option<&LocaleConfig>,
) -> Result<HashMap<String, Value>> {
    let tx = conn.transaction_immediate().context("Start transaction")?;
    let wh = RunnerWriteHooks::new(runner);
    let result =
        crate::service::delete_document_core(&tx, &wh, slug, id, def, user, locale_config)?;
    tx.commit().context("Commit transaction")?;

    // Clean up upload files after successful commit (skip for soft-delete to allow restore)
    if !def.soft_delete
        && let (Some(s), Some(fields)) = (storage, result.upload_doc_fields)
    {
        upload::delete_upload_files(s, &fields);
    }

    Ok(result.context)
}

/// Core restore logic on an existing connection: access check + restore row + FTS re-sync.
///
/// Checks trash access via `write_hooks.check_access`, then restores the document.
/// Does NOT manage transactions — caller must open/commit.
/// Returns the restored document on success.
pub fn restore_document_core(
    conn: &dyn DbConnection,
    write_hooks: &dyn crate::service::WriteHooks,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    user: Option<&Document>,
) -> Result<Document> {
    let access = write_hooks.check_access(def.access.resolve_trash(), user, Some(id), None)?;
    if matches!(access, crate::db::AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Restore access denied".into()));
    }

    let restored = query::restore(conn, slug, id)?;
    if !restored {
        return Err(ServiceError::NotFound(
            "Document not found or not deleted".into(),
        ));
    }

    // Re-sync FTS index (the FTS row was deleted on soft-delete)
    if conn.supports_fts()
        && let Ok(Some(doc)) = query::find_by_id_unfiltered(conn, slug, def, id, None)
    {
        query::fts::fts_upsert(conn, slug, &doc, Some(def))?;
    }

    query::find_by_id(conn, slug, def, id, None)?
        .ok_or_else(|| ServiceError::NotFound("Document not found after restore".into()))
}

/// Restore a soft-deleted document: clear `_deleted_at`, re-sync FTS index.
// Excluded from coverage: requires DB pool + FTS for full integration testing.
// Tested indirectly through admin handler and Lua API tests.
#[cfg(not(tarpaulin_include))]
pub fn restore_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    user: Option<&Document>,
) -> Result<Document> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;
    let wh = RunnerWriteHooks {
        runner,
        hooks_enabled: true,
        conn: Some(&tx),
    };

    let doc = restore_document_core(&tx, &wh, slug, id, def, user)?;

    tx.commit()?;
    Ok(doc)
}
