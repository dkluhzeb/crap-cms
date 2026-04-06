//! Collection CRUD orchestration: create, update, unpublish, delete.
//!
//! Each function wraps before-hooks → DB operation → after-hooks in a single transaction.

use std::collections::HashMap;

use anyhow::{Context as _, Result, anyhow};

use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document, upload, upload::StorageBackend},
    db::{DbConnection, DbPool, LocaleContext, query},
    hooks::{HookContext, HookEvent, HookRunner, ValidationCtx},
    service::{AfterChangeInput, WriteInput, WriteResult, build_hook_data, run_after_change_hooks},
};

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
    let is_draft = input.draft && def.has_drafts();

    let tx = conn.transaction_immediate().context("Start transaction")?;

    let ui_locale = input.ui_locale.as_deref();
    let hook_data = build_hook_data(&input.data, input.join_data);
    let hook_ctx = HookContext::builder(slug, "create")
        .data(hook_data)
        .locale(input.locale.clone())
        .draft(is_draft)
        .user(user)
        .ui_locale(ui_locale)
        .build();
    let val_ctx = ValidationCtx::builder(&tx, slug)
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(def.soft_delete)
        .build();
    let final_ctx = runner.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;
    let final_data = final_ctx.to_string_map(&def.fields);
    let mut persist_builder = crate::service::PersistOptions::builder()
        .password(input.password)
        .locale_ctx(input.locale_ctx)
        .draft(is_draft);
    if let Some(lctx) = input.locale_ctx {
        persist_builder = persist_builder.locale_config(&lctx.config);
    }
    let persist_opts = persist_builder.build();
    let doc = crate::service::persist_create(
        &tx,
        slug,
        def,
        &final_data,
        &final_ctx.data,
        &persist_opts,
    )?;

    let ctx = run_after_change_hooks(
        runner,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(slug, "create")
            .locale(input.locale)
            .draft(is_draft)
            .req_context(final_ctx.context)
            .user(user)
            .ui_locale(ui_locale)
            .build(),
        &tx,
    )?;

    tx.commit().context("Commit transaction")?;
    Ok((doc, ctx))
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
    let is_draft = input.draft && def.has_drafts();

    let tx = conn.transaction_immediate().context("Start transaction")?;

    let ui_locale = input.ui_locale.as_deref();
    let hook_data = build_hook_data(&input.data, input.join_data);
    let hook_ctx = HookContext::builder(slug, "update")
        .data(hook_data)
        .locale(input.locale.clone())
        .draft(is_draft)
        .user(user)
        .ui_locale(ui_locale)
        .build();
    let val_ctx = ValidationCtx::builder(&tx, slug)
        .exclude_id(Some(id))
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(def.soft_delete)
        .build();
    let final_ctx = runner.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;
    let final_data = final_ctx.to_string_map(&def.fields);

    let doc = if is_draft && def.has_versions() {
        crate::service::persist_draft_version(
            &tx,
            slug,
            id,
            def,
            &final_ctx.data,
            input.locale_ctx,
        )?
    } else {
        let mut update_builder = crate::service::PersistOptions::builder()
            .password(input.password)
            .locale_ctx(input.locale_ctx);
        if let Some(lctx) = input.locale_ctx {
            update_builder = update_builder.locale_config(&lctx.config);
        }
        crate::service::persist_update(
            &tx,
            slug,
            id,
            def,
            &final_data,
            &final_ctx.data,
            &update_builder.build(),
        )?
    };

    let ctx = run_after_change_hooks(
        runner,
        &def.hooks,
        &def.fields,
        &doc,
        AfterChangeInput::builder(slug, "update")
            .locale(input.locale)
            .draft(is_draft)
            .req_context(final_ctx.context)
            .user(user)
            .ui_locale(ui_locale)
            .build(),
        &tx,
    )?;

    tx.commit().context("Commit transaction")?;
    Ok((doc, ctx))
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
        .ok_or_else(|| anyhow!("Document {} not found in {}", id, slug))?;

    let hook_ctx = HookContext::builder(slug, "update")
        .data(doc.fields.clone())
        .draft(true)
        .locale(None::<String>)
        .user(user)
        .build();
    let final_ctx =
        runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, hook_ctx, &tx)?;

    crate::service::persist_unpublish(&tx, slug, id, def)?;

    run_after_change_hooks(
        runner,
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
    // For upload collections, load the document before deleting to get file paths
    let upload_doc_fields = if def.is_upload_collection() {
        let lc = locale_config.cloned().unwrap_or_default();
        let locale_ctx = LocaleContext::from_locale_string(None, &lc);

        let conn_ref: &dyn crate::db::DbConnection = conn;
        match query::find_by_id(conn_ref, slug, def, id, locale_ctx.as_ref()) {
            Ok(Some(doc)) => Some(doc.fields.clone()),
            Ok(None) => {
                tracing::warn!("Upload document {}/{} not found for file cleanup", slug, id);
                None
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to load upload document {}/{} for file cleanup: {}",
                    slug,
                    id,
                    e
                );
                None
            }
        }
    } else {
        None
    };

    let tx = conn.transaction_immediate().context("Start transaction")?;

    // Block deletion of documents that are referenced by other documents.
    // Only hard deletes are blocked — soft-deleted docs remain referenceable.
    if !def.soft_delete {
        let ref_count = query::ref_count::get_ref_count(&tx, slug, id)?.unwrap_or(0);
        if ref_count > 0 {
            anyhow::bail!(
                "Cannot delete: this document is referenced by {} other document(s)",
                ref_count
            );
        }
    }

    let mut hook_data: HashMap<String, Value> =
        [("id".to_string(), Value::String(id.to_string()))].into();
    if def.soft_delete {
        hook_data.insert("soft_delete".to_string(), Value::Bool(true));
    }

    let hook_ctx = HookContext::builder(slug, "delete")
        .data(hook_data.clone())
        .user(user)
        .build();
    let final_ctx =
        runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeDelete, hook_ctx, &tx)?;

    // Decrement ref counts on targets before hard delete (CASCADE would remove junction rows).
    // Soft delete does NOT adjust ref counts.
    if !def.soft_delete {
        let locale_cfg = locale_config.cloned().unwrap_or_default();
        query::ref_count::before_hard_delete(&tx, slug, id, &def.fields, &locale_cfg)?;
    }

    if def.soft_delete {
        let deleted = query::soft_delete(&tx, slug, id)?;
        if !deleted {
            anyhow::bail!(
                "Document '{}' not found in '{}' (or already deleted)",
                id,
                slug
            );
        }
    } else {
        let deleted = query::delete(&tx, slug, id)?;
        if !deleted {
            anyhow::bail!("Document '{}' not found in '{}'", id, slug);
        }
    }

    // Clean up FTS index in both hard-delete and soft-delete cases
    if tx.supports_fts() {
        query::fts::fts_delete(&tx, slug, id)?;
    }

    // Cancel pending image conversions for this document
    if def.is_upload_collection() {
        let _ = query::images::delete_entries_for_document(&tx, slug, id);
    }

    let after_ctx = HookContext::builder(slug, "delete")
        .data(hook_data)
        .context(final_ctx.context)
        .user(user)
        .build();
    let after_result =
        runner.run_hooks_with_conn(&def.hooks, HookEvent::AfterDelete, after_ctx, &tx)?;

    tx.commit().context("Commit transaction")?;

    // Clean up upload files after successful commit (skip for soft-delete to allow restore)
    if !def.soft_delete
        && let (Some(s), Some(fields)) = (storage, upload_doc_fields)
    {
        upload::delete_upload_files(s, &fields);
    }

    Ok(after_result.context)
}

/// Restore a soft-deleted document: clear `_deleted_at`, re-sync FTS index.
// Excluded from coverage: requires DB pool + FTS for full integration testing.
// Tested indirectly through admin handler and Lua API tests.
#[cfg(not(tarpaulin_include))]
pub fn restore_document(
    pool: &DbPool,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
) -> Result<Document> {
    let mut conn = pool.get().context("DB connection")?;
    let tx = conn.transaction_immediate().context("Start transaction")?;

    let restored = query::restore(&tx, slug, id)?;
    if !restored {
        anyhow::bail!("Document not found or not deleted");
    }

    // Re-sync FTS index (the FTS row was deleted on soft-delete)
    if tx.supports_fts()
        && let Ok(Some(doc)) = query::find_by_id_unfiltered(&tx, slug, def, id, None)
    {
        query::fts::fts_upsert(&tx, slug, &doc, Some(def))?;
    }

    tx.commit()?;

    query::find_by_id(&conn, slug, def, id, None)?
        .ok_or_else(|| anyhow!("Document not found after restore"))
}
