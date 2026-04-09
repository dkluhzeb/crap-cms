//! Core delete operation for collections.

use std::collections::HashMap;

use serde_json::Value;

use crate::{
    config::LocaleConfig,
    core::{CollectionDefinition, Document},
    db::{AccessResult, DbConnection, LocaleContext, query},
    hooks::{HookContext, HookEvent},
    service::hooks::WriteHooks,
};

use super::ServiceError;

type Result<T> = std::result::Result<T, ServiceError>;

/// Result of a delete operation.
pub struct DeleteResult {
    /// Request-scoped context returned by after-delete hooks.
    pub context: HashMap<String, Value>,
    /// Upload file fields from the deleted document (for post-commit cleanup).
    pub upload_doc_fields: Option<HashMap<String, Value>>,
}

/// Delete a document on an existing connection/transaction.
///
/// Runs the full lifecycle: ref count check -> before-delete hooks -> delete -> cleanup -> after-delete hooks.
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
    if matches!(access, AccessResult::Denied) {
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
