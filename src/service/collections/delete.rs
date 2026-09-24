//! Collection document deletion.

use crate::{
    config::LocaleConfig,
    core::{
        ReqContext,
        upload::{self, StorageBackend},
    },
    service::{
        ServiceContext, ServiceError, delete_document_in_conn, invalidate_user_streams_if_auth,
        run_pool_write, warn_orphaned_files,
    },
};

type Result<T> = std::result::Result<T, ServiceError>;

/// Delete a document: before-hooks -> delete -> after-hooks.
///
/// **Pool mode** (`ctx.pool` set): opens a transaction, commits after success.
/// **Conn mode** (`ctx.conn` set, Lua CRUD path): runs on the existing connection.
///
/// Upload files of a hard delete are removed only once the delete is durable:
/// pool mode deletes them through `storage` after its own commit; conn mode
/// hands them to the enclosing transaction's cleanup queue (`ctx.file_cleanup`)
/// and, with none, leaves them in storage rather than delete them before the
/// caller commits.
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
        delete_document_conn(ctx, id, locale_config)
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

    // Post-commit: the rows are durably gone, so their files go now.
    delete_committed_files(ctx, storage, &result.upload_keys);

    Ok(result.context)
}

/// Delete the files of a delete this operation already committed.
///
/// `keys` covers the published row's files AND every version snapshot's, and is
/// empty for a soft delete, so an undelete still finds them. The write resolves
/// them while the row and its snapshots still exist — the cross-collection
/// queue stores keys, not documents, so this side no longer needs the
/// collection's upload config.
fn delete_committed_files(
    ctx: &ServiceContext,
    storage: Option<&dyn StorageBackend>,
    keys: &[String],
) {
    if keys.is_empty() {
        return;
    }

    let Some(s) = storage else {
        warn_orphaned_files(ctx.slug, "storage backend", keys.len());

        return;
    };

    upload::delete_storage_keys(s, keys);
}

/// Hand the files of a delete made INSIDE the caller's transaction to that
/// transaction's post-commit cleanup queue. Deleting the bytes now and then
/// rolling back would leave the restored row pointing at nothing, so without a
/// queue the files stay: an orphaned file is the safe direction.
fn queue_files_for_commit(ctx: &ServiceContext, keys: Vec<String>) {
    if keys.is_empty() {
        return;
    }

    let Some(queue) = &ctx.file_cleanup else {
        warn_orphaned_files(ctx.slug, "post-commit file cleanup queue", keys.len());

        return;
    };

    queue.borrow_mut().extend(keys);
}

/// Conn-based delete: uses existing connection (Lua CRUD path).
fn delete_document_conn(
    ctx: &ServiceContext,
    id: &str,
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

    queue_files_for_commit(ctx, result.upload_keys);

    Ok(result.context)
}
