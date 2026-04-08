//! Core write operations for collections, accepting `&dyn WriteHooks` for hook abstraction.
//!
//! These functions operate on an existing connection/transaction. The caller is responsible
//! for transaction management (open, commit/rollback). This allows both pool-based callers
//! (admin, gRPC, MCP) and in-transaction callers (Lua CRUD) to share the same code.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document},
    db::{DbConnection, LocaleContext, query},
    hooks::{HookContext, HookEvent, ValidationCtx},
    service::{
        AfterChangeInput, PersistOptions, WriteInput, WriteResult, build_hook_data, persist_create,
        persist_draft_version, persist_update, run_after_change_hooks,
    },
};

use super::{ServiceError, write_hooks::WriteHooks};

type Result<T> = std::result::Result<T, ServiceError>;

/// Strip denied field names from flat data and return a (potentially cloned) join_data map
/// with denied keys removed. If no fields are denied, returns the original join_data unchanged.
pub(crate) fn strip_denied_fields<'a>(
    denied: &[String],
    data: &mut HashMap<String, String>,
    join_data: &'a HashMap<String, Value>,
) -> std::borrow::Cow<'a, HashMap<String, Value>> {
    if denied.is_empty() {
        return std::borrow::Cow::Borrowed(join_data);
    }

    for name in denied {
        data.remove(name);
    }

    let mut filtered = join_data.clone();
    for name in denied {
        filtered.remove(name);
    }
    std::borrow::Cow::Owned(filtered)
}

/// Context for a validate-only run (no persist).
pub struct ValidateContext<'a> {
    pub slug: &'a str,
    /// Table name for unique checks — collection slug or `_global_{slug}`.
    pub table_name: &'a str,
    pub fields: &'a [crate::core::FieldDefinition],
    pub hooks: &'a crate::core::collection::Hooks,
    pub operation: &'a str,
    /// Exclude this document from unique checks (update path).
    pub exclude_id: Option<&'a str>,
    pub soft_delete: bool,
}

/// Validate a document without persisting — runs the full before-write pipeline
/// (field stripping, field hooks, validation, collection hooks) and returns.
///
/// Used by live validation endpoints.
pub fn validate_document(
    conn: &dyn DbConnection,
    write_hooks: &dyn WriteHooks,
    ctx: &ValidateContext<'_>,
    mut input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<()> {
    // Note: collection-level access check is intentionally skipped here.
    // Validation endpoints already check access before calling this function.

    let is_draft = input.draft;

    // Strip write-denied fields
    let denied = write_hooks.field_write_denied(ctx.fields, user, ctx.operation);
    let join_data = strip_denied_fields(&denied, &mut input.data, input.join_data);

    let hook_data = build_hook_data(&input.data, &join_data);
    let hook_ctx = HookContext::builder(ctx.slug, ctx.operation)
        .data(hook_data)
        .locale(input.locale.clone())
        .draft(is_draft)
        .user(user)
        .build();

    let val_ctx = ValidationCtx::builder(conn, ctx.table_name)
        .exclude_id(ctx.exclude_id)
        .draft(is_draft)
        .locale_ctx(input.locale_ctx)
        .soft_delete(ctx.soft_delete)
        .build();

    write_hooks.run_before_write(ctx.hooks, ctx.fields, hook_ctx, &val_ctx)?;

    Ok(())
}

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
    mut input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<WriteResult> {
    // Collection-level access check
    let access = write_hooks.check_access(def.access.create.as_deref(), user, None, None)?;
    if matches!(access, crate::db::AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Create access denied".into()));
    }

    let is_draft = input.draft && def.has_drafts();
    let ui_locale = input.ui_locale.as_deref();

    // Strip write-denied fields before hook processing
    let denied = write_hooks.field_write_denied(&def.fields, user, "create");
    let join_data = strip_denied_fields(&denied, &mut input.data, input.join_data);

    let hook_data = build_hook_data(&input.data, &join_data);
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

    let doc = persist_create(
        conn,
        slug,
        def,
        &final_data,
        &final_ctx.data,
        &persist_builder.build(),
    )?;

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

    // Hydrate join fields (arrays, blocks, has-many) so the returned document is complete
    let mut doc = doc;
    query::hydrate_document(conn, slug, &def.fields, &mut doc, None, input.locale_ctx)?;

    // Strip read-denied fields AFTER hydration (hydration can add join data for denied fields)
    let read_denied = write_hooks.field_read_denied(&def.fields, user);
    for name in &read_denied {
        doc.fields.remove(name);
    }

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
    mut input: WriteInput<'_>,
    user: Option<&Document>,
) -> Result<WriteResult> {
    // Collection-level access check
    let access = write_hooks.check_access(def.access.update.as_deref(), user, Some(id), None)?;
    if matches!(access, crate::db::AccessResult::Denied) {
        return Err(ServiceError::AccessDenied("Update access denied".into()));
    }

    let is_draft = input.draft && def.has_drafts();
    let ui_locale = input.ui_locale.as_deref();

    // Strip write-denied fields before hook processing
    let denied = write_hooks.field_write_denied(&def.fields, user, "update");
    let join_data = strip_denied_fields(&denied, &mut input.data, input.join_data);

    let hook_data = build_hook_data(&input.data, &join_data);
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
        persist_update(
            conn,
            slug,
            id,
            def,
            &final_data,
            &final_ctx.data,
            &update_builder.build(),
        )?
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

    // Hydrate join fields (arrays, blocks, has-many) so the returned document is complete
    let mut doc = doc;
    query::hydrate_document(conn, slug, &def.fields, &mut doc, None, input.locale_ctx)?;

    // Strip read-denied fields AFTER hydration
    let read_denied = write_hooks.field_read_denied(&def.fields, user);
    for name in &read_denied {
        doc.fields.remove(name);
    }

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
    // Collection-level access check — use trash access for soft delete, delete for hard
    let access_ref = if def.soft_delete {
        def.access.resolve_trash()
    } else {
        def.access.delete.as_deref()
    };
    let access = write_hooks.check_access(access_ref, user, Some(id), None)?;
    if matches!(access, crate::db::AccessResult::Denied) {
        let msg = if def.soft_delete {
            "Trash access denied"
        } else {
            "Delete access denied"
        };
        return Err(ServiceError::AccessDenied(msg.into()));
    }

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
            return Err(ServiceError::Referenced {
                id: id.to_string(),
                count: ref_count,
            });
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
    let final_ctx =
        write_hooks.run_hooks_with_conn(&def.hooks, HookEvent::BeforeDelete, hook_ctx, conn)?;

    // Decrement ref counts before hard delete
    if !def.soft_delete {
        let locale_cfg = locale_config.cloned().unwrap_or_default();
        query::ref_count::before_hard_delete(conn, slug, id, &def.fields, &locale_cfg)?;
    }

    // Execute delete
    if def.soft_delete {
        let deleted = query::soft_delete(conn, slug, id)?;
        if !deleted {
            return Err(ServiceError::NotFound(format!(
                "Document '{id}' not found in '{slug}' (or already deleted)"
            )));
        }
    } else {
        let deleted = query::delete(conn, slug, id)?;
        if !deleted {
            return Err(ServiceError::NotFound(format!(
                "Document '{id}' not found in '{slug}'"
            )));
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
    let after_result =
        write_hooks.run_hooks_with_conn(&def.hooks, HookEvent::AfterDelete, after_ctx, conn)?;

    Ok(DeleteResult {
        context: after_result.context,
        upload_doc_fields,
    })
}
