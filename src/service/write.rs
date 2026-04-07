//! Core write operations for collections, accepting `&dyn WriteHooks` for hook abstraction.
//!
//! These functions operate on an existing connection/transaction. The caller is responsible
//! for transaction management (open, commit/rollback). This allows both pool-based callers
//! (admin, gRPC, MCP) and in-transaction callers (Lua CRUD) to share the same code.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document},
    db::{DbConnection, LocaleContext, query},
    hooks::{HookContext, HookEvent, ValidationCtx},
    service::{
        AfterChangeInput, PersistOptions, WriteInput, WriteResult,
        build_hook_data, persist_create, persist_draft_version, persist_update,
        run_after_change_hooks,
    },
};

use super::write_hooks::WriteHooks;

/// Result of a delete operation.
pub struct DeleteResult {
    /// Request-scoped context returned by after-delete hooks.
    pub context: HashMap<String, Value>,
    /// Upload file fields from the deleted document (for post-commit cleanup).
    pub upload_doc_fields: Option<HashMap<String, Value>>,
}

/// Create a document on an existing connection/transaction.
///
/// Runs the full lifecycle: before-write hooks → persist → after-write hooks.
/// Does NOT manage transactions — caller must open/commit.
pub fn create_document_core(
    conn: &dyn DbConnection,
    write_hooks: &dyn WriteHooks,
    slug: &str,
    def: &CollectionDefinition,
    input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<WriteResult> {
    let is_draft = input.draft && def.has_drafts();
    let ui_locale = input.ui_locale.as_deref();

    let hook_data = build_hook_data(&input.data, input.join_data);
    let hook_ctx = HookContext::builder(slug, "create")
        .data(hook_data)
        .locale(input.locale.clone())
        .draft(is_draft)
        .user(user)
        .ui_locale(ui_locale)
        .build();

    let val_ctx = ValidationCtx::builder(conn, slug)
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(def.soft_delete)
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;
    let final_data = final_ctx.to_string_map(&def.fields);

    let mut persist_builder = PersistOptions::builder()
        .password(input.password)
        .locale_ctx(input.locale_ctx)
        .draft(is_draft);
    if let Some(lctx) = input.locale_ctx {
        persist_builder = persist_builder.locale_config(&lctx.config);
    }

    let doc = persist_create(conn, slug, def, &final_data, &final_ctx.data, &persist_builder.build())?;

    let ctx = run_after_change_hooks(
        write_hooks,
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
        conn,
    )?;

    Ok((doc, ctx))
}

/// Update a document on an existing connection/transaction.
///
/// Runs the full lifecycle: before-write hooks → persist → after-write hooks.
/// Handles draft-only version saves when `input.draft` is true.
/// Does NOT manage transactions — caller must open/commit.
pub fn update_document_core(
    conn: &dyn DbConnection,
    write_hooks: &dyn WriteHooks,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<WriteResult> {
    let is_draft = input.draft && def.has_drafts();
    let ui_locale = input.ui_locale.as_deref();

    let hook_data = build_hook_data(&input.data, input.join_data);
    let hook_ctx = HookContext::builder(slug, "update")
        .data(hook_data)
        .locale(input.locale.clone())
        .draft(is_draft)
        .user(user)
        .ui_locale(ui_locale)
        .build();

    let val_ctx = ValidationCtx::builder(conn, slug)
        .exclude_id(Some(id))
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(def.soft_delete)
        .build();

    let final_ctx = write_hooks.run_before_write(&def.hooks, &def.fields, hook_ctx, &val_ctx)?;
    let final_data = final_ctx.to_string_map(&def.fields);

    let doc = if is_draft && def.has_versions() {
        persist_draft_version(conn, slug, id, def, &final_ctx.data, input.locale_ctx)?
    } else {
        let mut update_builder = PersistOptions::builder()
            .password(input.password)
            .locale_ctx(input.locale_ctx);
        if let Some(lctx) = input.locale_ctx {
            update_builder = update_builder.locale_config(&lctx.config);
        }
        persist_update(conn, slug, id, def, &final_data, &final_ctx.data, &update_builder.build())?
    };

    let ctx = run_after_change_hooks(
        write_hooks,
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
        conn,
    )?;

    Ok((doc, ctx))
}

/// Delete a document on an existing connection/transaction.
///
/// Runs the full lifecycle: ref count check → before-delete hooks → delete → cleanup → after-delete hooks.
/// Does NOT manage transactions — caller must open/commit.
/// Upload file cleanup is returned as `upload_doc_fields` for the caller to handle after commit.
pub fn delete_document_core(
    conn: &dyn DbConnection,
    write_hooks: &dyn WriteHooks,
    slug: &str,
    id: &str,
    def: &CollectionDefinition,
    user: Option<&Document>,
    locale_config: Option<&LocaleConfig>,
) -> Result<DeleteResult> {
    // Pre-load upload doc for file cleanup (before deletion removes it)
    let upload_doc_fields = if def.is_upload_collection() {
        let lc = locale_config.cloned().unwrap_or_default();
        let locale_ctx = LocaleContext::from_locale_string(None, &lc);
        query::find_by_id(conn, slug, def, id, locale_ctx.as_ref())
            .ok()
            .flatten()
            .map(|d| d.fields.clone())
    } else {
        None
    };

    // Ref count protection (hard delete only)
    if !def.soft_delete {
        let ref_count = query::ref_count::get_ref_count(conn, slug, id)?.unwrap_or(0);
        if ref_count > 0 {
            anyhow::bail!(
                "Cannot delete: this document is referenced by {} other document(s)",
                ref_count
            );
        }
    }

    // Before-delete hooks
    let mut hook_data: HashMap<String, Value> =
        [("id".to_string(), Value::String(id.to_string()))].into();
    if def.soft_delete {
        hook_data.insert("soft_delete".to_string(), Value::Bool(true));
    }

    let hook_ctx = HookContext::builder(slug, "delete")
        .data(hook_data.clone())
        .user(user)
        .build();
    let final_ctx = write_hooks.run_hooks_with_conn(&def.hooks, HookEvent::BeforeDelete, hook_ctx, conn)?;

    // Decrement ref counts before hard delete
    if !def.soft_delete {
        let locale_cfg = locale_config.cloned().unwrap_or_default();
        query::ref_count::before_hard_delete(conn, slug, id, &def.fields, &locale_cfg)?;
    }

    // Execute delete
    if def.soft_delete {
        let deleted = query::soft_delete(conn, slug, id)?;
        if !deleted {
            anyhow::bail!("Document '{}' not found in '{}' (or already deleted)", id, slug);
        }
    } else {
        let deleted = query::delete(conn, slug, id)?;
        if !deleted {
            anyhow::bail!("Document '{}' not found in '{}'", id, slug);
        }
    }

    // Cleanup
    if conn.supports_fts() {
        query::fts::fts_delete(conn, slug, id)?;
    }
    if def.is_upload_collection() {
        let _ = query::images::delete_entries_for_document(conn, slug, id);
    }

    // After-delete hooks
    let after_ctx = HookContext::builder(slug, "delete")
        .data(hook_data)
        .context(final_ctx.context)
        .user(user)
        .build();
    let after_result = write_hooks.run_hooks_with_conn(&def.hooks, HookEvent::AfterDelete, after_ctx, conn)?;

    Ok(DeleteResult {
        context: after_result.context,
        upload_doc_fields,
    })
}
