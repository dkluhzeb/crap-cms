//! Collection document deletion.

use crate::{
    config::LocaleConfig,
    core::{
        ReqContext,
        upload::{self, StorageBackend},
    },
    service::{
        ServiceContext, ServiceError, delete_document_in_conn, invalidate_user_streams_if_auth,
        run_pool_write,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Delete a document: before-hooks -> delete -> after-hooks.
///
/// **Pool mode** (`ctx.pool` set): opens a transaction, commits after success.
/// **Conn mode** (`ctx.conn` set, Lua CRUD path): runs on the existing connection.
///
/// # Errors
///
/// Returns service-layer errors (access denied, ref-count protection, hook
/// errors) or a backend error if the DB transaction or persistence fails.
#[cfg(not(tarpaulin_include))]
pub fn delete_document(
    ctx: &ServiceContext,
    id: &str,
    storage: Option<&dyn StorageBackend>,
    locale_config: Option<&LocaleConfig>,
) -> Result<ReqContext> {
    if ctx.pool.is_some() {
        delete_document_pool(ctx, id, storage, locale_config)
    } else {
        delete_document_conn(ctx, id, storage, locale_config)
    }
}

/// Pool-based delete: own transaction with event publishing after commit.
fn delete_document_pool(
    ctx: &ServiceContext,
    id: &str,
    storage: Option<&dyn StorageBackend>,
    locale_config: Option<&LocaleConfig>,
) -> Result<ReqContext> {
    let result = run_pool_write(
        ctx,
        None,
        |inner| delete_document_in_conn(inner, id, locale_config),
        |ctx, result| {
            ctx.publish_delete_event(id, result.event.clone());
            // Deleting an auth document revokes that user — tear down their live streams
            // post-commit. This applies to BOTH hard and soft delete: the per-request
            // evaluator resolves users via `find_by_id`, which excludes soft-deleted
            // rows, so a trashed user is already rejected (`UserMissing`) on new
            // requests; their open SSE/subscribe streams (which never re-resolve) must be
            // torn down too. No-op for non-auth collections.
            invalidate_user_streams_if_auth(ctx, id);
        },
    )?;

    clean_up_files(ctx, storage, result.upload_keys);

    Ok(result.context)
}

/// Hand the deleted document's files over for cleanup after the commit.
///
/// In conn mode this runs INSIDE the caller's transaction — deleting the bytes
/// now and then rolling back would leave the restored row pointing at nothing —
/// so with an enclosing scope the keys are queued for its post-commit flush;
/// without one (direct conn callers) they are deleted immediately.
///
/// `keys` covers the published row's files AND every version snapshot's, and is
/// empty for a soft delete, so an undelete still finds them. The write resolves
/// them while the row and its snapshots still exist — the cross-collection
/// queue stores keys, not documents, so this side no longer needs the
/// collection's upload config.
fn clean_up_files(ctx: &ServiceContext, storage: Option<&dyn StorageBackend>, keys: Vec<String>) {
    if keys.is_empty() {
        return;
    }

    if let Some(queue) = &ctx.file_cleanup {
        queue.borrow_mut().extend(keys);

        return;
    }

    if let Some(s) = storage {
        upload::delete_storage_keys(s, &keys);
    }
}

/// Conn-based delete: uses existing connection (Lua CRUD path).
fn delete_document_conn(
    ctx: &ServiceContext,
    id: &str,
    storage: Option<&dyn StorageBackend>,
    locale_config: Option<&LocaleConfig>,
) -> Result<ReqContext> {
    let result = delete_document_in_conn(ctx, id, locale_config)?;

    ctx.clear_cache();

    ctx.publish_delete_event(id, result.event);
    // Deleting an auth document revokes that user — tear down their live streams
    // (conn mode fires immediate). Applies to both hard and soft delete: a
    // soft-deleted user is rejected by the evaluator's `find_by_id` on new
    // requests, so their open streams must be closed too. See the pool path.
    invalidate_user_streams_if_auth(ctx, id);

    clean_up_files(ctx, storage, result.upload_keys);

    Ok(result.context)
}
