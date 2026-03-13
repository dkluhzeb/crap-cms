//! Collection CRUD orchestration: create, update, unpublish, delete.
//!
//! Each function wraps before-hooks → DB operation → after-hooks in a single transaction.

use std::collections::HashMap;

use anyhow::{Context as _, Result, anyhow};

use serde_json::Value;

use rusqlite::TransactionBehavior;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, document::Document, upload},
    db::{
        DbPool,
        query::{self, LocaleContext},
    },
    hooks::lifecycle::{HookContext, HookEvent, HookRunner},
};

use super::{WriteInput, WriteResult, build_before_ctx, build_hook_data, run_after_change_hooks};

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
    let is_draft = input.draft && def.has_drafts();

    let mut conn = pool.get().context("DB connection")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("Start transaction")?;

    let ui_locale = input.ui_locale.as_deref();
    let hook_data = build_hook_data(&input.data, input.join_data);
    let hook_ctx = build_before_ctx(
        slug,
        "create",
        hook_data,
        input.locale.clone(),
        is_draft,
        user,
        ui_locale,
    );
    let final_ctx = runner.run_before_write(
        &def.hooks,
        &def.fields,
        hook_ctx,
        &tx,
        slug,
        None,
        input.locale_ctx,
    )?;
    let final_data = final_ctx.to_string_map(&def.fields);
    let persist_opts = super::PersistOptions {
        password: input.password,
        locale_ctx: input.locale_ctx,
        is_draft,
    };
    let doc = super::persist_create(&tx, slug, def, &final_data, &final_ctx.data, &persist_opts)?;

    let ctx = run_after_change_hooks(
        runner,
        &def.hooks,
        &def.fields,
        slug,
        "create",
        &doc,
        input.locale,
        is_draft,
        final_ctx.context,
        &tx,
        user,
        ui_locale,
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
    let is_draft = input.draft && def.has_drafts();

    let mut conn = pool.get().context("DB connection")?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("Start transaction")?;

    let ui_locale = input.ui_locale.as_deref();
    let hook_data = build_hook_data(&input.data, input.join_data);
    let hook_ctx = build_before_ctx(
        slug,
        "update",
        hook_data,
        input.locale.clone(),
        is_draft,
        user,
        ui_locale,
    );
    let final_ctx = runner.run_before_write(
        &def.hooks,
        &def.fields,
        hook_ctx,
        &tx,
        slug,
        Some(id),
        input.locale_ctx,
    )?;
    let final_data = final_ctx.to_string_map(&def.fields);

    let doc = if is_draft && def.has_versions() {
        super::persist_draft_version(&tx, slug, id, def, &final_ctx.data, input.locale_ctx)?
    } else {
        super::persist_update(
            &tx,
            slug,
            id,
            def,
            &final_data,
            &final_ctx.data,
            input.password,
            input.locale_ctx,
        )?
    };

    let ctx = run_after_change_hooks(
        runner,
        &def.hooks,
        &def.fields,
        slug,
        "update",
        &doc,
        input.locale,
        is_draft,
        final_ctx.context,
        &tx,
        user,
        ui_locale,
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
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("Start transaction")?;

    let doc = query::find_by_id_raw(&tx, slug, def, id, None)?
        .ok_or_else(|| anyhow!("Document {} not found in {}", id, slug))?;

    let hook_ctx = build_before_ctx(slug, "update", doc.fields.clone(), None, false, user, None);
    let final_ctx =
        runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeChange, hook_ctx, &tx)?;

    super::persist_unpublish(&tx, slug, id, def)?;

    run_after_change_hooks(
        runner,
        &def.hooks,
        &def.fields,
        slug,
        "update",
        &doc,
        None,
        false,
        final_ctx.context,
        &tx,
        user,
        None,
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
pub fn delete_document(
    pool: &DbPool,
    runner: &HookRunner,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    user: Option<&Document>,
    config_dir: Option<&std::path::Path>,
) -> Result<HashMap<String, Value>> {
    let mut conn = pool.get().context("DB connection")?;

    // For upload collections, load the document before deleting to get file paths
    let upload_doc_fields = if def.is_upload_collection() {
        let locale_ctx = LocaleContext::from_locale_string(None, &LocaleConfig::default());

        query::find_by_id(&conn, slug, def, id, locale_ctx.as_ref())
            .ok()
            .flatten()
            .map(|doc| doc.fields.clone())
    } else {
        None
    };

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .context("Start transaction")?;

    let hook_ctx = HookContext::builder(slug, "delete")
        .data([("id".to_string(), Value::String(id.to_string()))].into())
        .user(user)
        .build();
    let final_ctx =
        runner.run_hooks_with_conn(&def.hooks, HookEvent::BeforeDelete, hook_ctx, &tx)?;

    query::delete(&tx, slug, id)?;

    query::fts::fts_delete(&tx, slug, id)?;

    let after_ctx = HookContext::builder(slug, "delete")
        .data([("id".to_string(), Value::String(id.to_string()))].into())
        .context(final_ctx.context)
        .user(user)
        .build();
    let after_result =
        runner.run_hooks_with_conn(&def.hooks, HookEvent::AfterDelete, after_ctx, &tx)?;

    tx.commit().context("Commit transaction")?;

    // Clean up upload files after successful commit
    if let (Some(dir), Some(fields)) = (config_dir, upload_doc_fields) {
        upload::delete_upload_files(dir, &fields);
    }

    Ok(after_result.context)
}
